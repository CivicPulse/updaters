# Results collector Helm chart

This chart runs the durable queue consumer. Supabase owns the five-minute source
scheduler. It creates no CronJob, public Service, or Ingress. Database/R2 state is
durable; `/tmp` is bounded, disk-backed scratch and may disappear at any restart.

Use context `do-atl1-k8s-1-37-0-do-2-atl1-1793724241236` for every environment:

| Environment | Namespace | R2 bucket | Public file origin |
| --- | --- | --- | --- |
| dev | civpulse-dev | civpulse-files-dev | https://files-dev.civpulse.org |
| test | civpulse-test | civpulse-files-test | https://files-test.civpulse.org |
| prod | civpulse-prod | civpulse-files | https://files.civpulse.org |

The dev/test buckets and public domains are separate from production. Only
approved public source data or synthetic fixtures belong there. Curated mappings,
private metadata, tokens, and application credentials must never enter public assets.

## Install disabled

The namespace must match `environment`, and `image.digest` must be the immutable
SHA-256 digest emitted by a successful scanned image publication. The default
image repository is `registry.digitalocean.com/hatchk8s/civpulse-results-collector`.
The default image digest is deliberately empty: an unreviewed image cannot install.
The all-`a` digest used in CI is a rendering fixture, never a deployment value.

```sh
kubectl --context do-atl1-k8s-1-37-0-do-2-atl1-1793724241236 apply -f deploy/environments/namespaces.yaml
helm upgrade --install results deploy/helm/civpulse-results-collector \
  --kube-context do-atl1-k8s-1-37-0-do-2-atl1-1793724241236 \
  --namespace civpulse-test -f deploy/environments/test.yaml \
  --set-string image.digest=sha256:<scanned-manifest-digest> \
  --wait --timeout 5m
```

Repeat with the dev/prod namespace and corresponding environment file. Collection
is false in all committed environment values. A disabled daemon stays healthy
without making database, SOS, or R2 requests. Credentials may be absent while
disabled. Startup/readiness/liveness execute the local heartbeat command; a
remote database outage must not trigger a restart loop. `health` separately checks
service-only database access and should be monitored operationally.

The initial envelope is requests 100m CPU/512Mi RAM, limits 1 CPU/1Gi RAM,
2Gi disk-backed scratch, and 3Gi ephemeral-storage limit. Retain measurement
headroom and revise these values from real export/pipeline measurements before
activation. The reviewed enlarged offline pipeline reached about 403.6 MiB process
RSS, which excludes page cache and other cgroup charges; a 512Mi memory limit is
insufficient production headroom. The existing nodes can accommodate the 1Gi
limit; this chart does not add or resize nodes. `Recreate` stops the old worker before starting a replacement; the
queue's fenced leases still protect accidental overlaps. The pod gets 120 seconds
to stop claiming work and drain. It has no Kubernetes API token or inbound ports.

## Provisional resource envelope and acceptance gate

The 512Mi request / 1Gi limit / 2Gi scratch settings are provisional, not a claim
that the worker supports every source up to its 512Mi raw / 64Mi per-scope caps
within those limits. The local 46MB and 174MB receipts measure process RSS and
sampled logical disk; they do not measure cgroup memory including page cache,
eviction pressure, or worst-case asset generation and replay.

Before activation under [issue #9](https://github.com/CivicPulse/updaters/issues/9):

- Run the actual scanned, digest-pinned image with the proposed cgroup CPU/memory
  and disk-backed scratch limits. Record peak cgroup memory, worker disk usage,
  runtime, OOM/restart/eviction events, and remaining headroom.
- Check schedulable capacity with dev/test Supabase and all existing pods present;
  namespace separation reserves no node memory. Do not add or resize nodes as an
  incidental chart install.
- Exercise representative inputs near the accepted 512Mi raw / 64Mi scope caps,
  including complete assets and original-receipt replay. The 174MB fixture alone
  does not establish that maximum envelope. If the accepted envelope cannot fit,
  lower configurable admission/concurrency limits with reviewed implementation
  and tests, or obtain a reviewed capacity increase, then repeat acceptance.
- Start at one job per worker. Increasing concurrency requires new measurements.
- Record the exact digest tested and use that same digest in each Helm release;
  inspect the running pod image ID against it. A successful scaffold `--check`,
  disabled-daemon smoke, or chart dry run does not establish pipeline acceptance.

Keep every production collection flag disabled until those checks and the full
publication/recovery/readback acceptance are complete. The chart installs no
resource-admission exceptions, automatic node changes, or production enablement.

## Required secrets

Create separately in each namespace; do not put values in Helm files, logs, PRs,
image layers, or shell arguments:

| Secret | Keys |
| --- | --- |
| registry-hatchk8s | Kubernetes dockerconfigjson pull credentials for the DigitalOcean registry |
| results-supabase | SUPABASE_URL, SUPABASE_SERVICE_ROLE_KEY |
| results-r2 | R2_ENDPOINT, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY |

Only these individual secret keys are injected; extra keys cannot override the
chart's activation/environment configuration. Enabled pods require every key.
Use credentials restricted to the corresponding bucket's object read/write,
without bucket deletion or account administration. Rotate via the secret store
and restart the Deployment. Do not share dev/test database or R2 credentials with
production. See `../../supabase/README.md` for isolated self-hosted environments.

## Controlled once run and activation

Before an import, configure the reviewed existing election UUID/date/exact URL,
expected jurisdiction/item counts, and explicit polling window through the
website's administrator contract. Install those reviewed values while keeping
`collection.enabled=false`. Neither the chart nor a Helm upgrade creates a feed,
rewrites election identities, stores poll/rebuild Vault secrets, or enables cron.

For a controlled run, create a separate Kubernetes Job from the reviewed rendered
Deployment's `spec.template`: remove Deployment labels/selectors, set
`restartPolicy: Never`, use command argument `once`, set `backoffLimit: 0` and
`activeDeadlineSeconds: 900`, make all secret references required, and explicitly
set `SOS_COLLECTOR_ENABLED=true` in that Job's environment. Preserve all security,
resource, namespace, scratch, and network-policy selector settings. Review the
Job manifest before applying it. This is one queued job, not a new scheduler.
Pause the feed again when investigating a failure. Deleting scratch does not
remove evidence or durable import receipts.

Keep production disabled until the acceptance evidence is complete. Enabling
production requires both `acceptance.approved=true` and a nonempty
`acceptance.reference` linking the recorded checks/operator, in addition to
reviewed election identity/counts and required credentials. These are deliberate
operator inputs; the committed prod file leaves both unset. They do not replace
real production acceptance or the database feed enable/window controls.

For rollback, pause the feed through the administrator RPC, drain the current
collector, then `helm rollback results <known-good-release>` in the same namespace.
Inspect `helm get values --all` first: never let rollback restore an enabled feed
unexpectedly. Keep the database feed paused until the restored worker and public
pointer have been verified. Retain all published raw data and immutable assets.

## Validation

`python3 .github/scripts/test-chart.py` runs strict Helm lint, checks security,
namespace/bucket/origin wiring and probes, and proves invalid digests, wrong
namespaces, incomplete configuration, and unaccepted production activation fail.
Container CI additionally starts the real disabled daemon with no network, a
writable disk scratch bind, read-only root, non-root user, dropped capabilities,
and the production memory limit. It checks the heartbeat/liveness command and
clean SIGTERM exit within 15 seconds. Publishing scans and pushes that same
validated image without rebuilding it. CI scans rendered manifests with Trivy and validates them with server-side dry
runs against a digest-pinned Kubernetes 1.37 Kind cluster. That API validation does not establish a
running collector or production acceptance; the real-cluster disabled rollout and
controlled recovery/readback checks remain release gates.

Byte-cap values (`collection.maxDownloadBytes` and `maxScopeBytes`) remain numeric
integers in Helm values. The ConfigMap explicitly formats them as decimal integer
strings, including defaults and lower admission overrides; the Rust parser stays
strict and rejects scientific notation. CI renders YAML overrides and passes the
resulting strings through the worker’s actual admission implementation.

The monitor supports independent email and ntfy channels. `monitoring.ntfyEnabled`
defaults to false; the production values opt into the exact public topic
`https://ntfy.sh/CivPulse` through `monitoring.ntfyUrl`. Other environments and
endpoints are rejected. This setting neither enables the monitor itself nor
changes collection. Preserve the live monitor/email/collection values when
upgrading an active release; committed production defaults still disable collection.

The retained monitor PVC stores `/state/alerts.json` for email and
`/state/ntfy.json` for push notifications. Neither channel's failure advances the
other channel's deduplication state. New-data notifications use confirmed signed-S3
pointer/database agreement and the semantic fingerprint, not source polls or raw
file hashes. See the [notification runbook](../../../docs/operations/results-ntfy.md).
