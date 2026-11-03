# Georgia results collector operations

Parent feature: [updaters #1](https://github.com/CivicPulse/updaters/issues/1).
Operational acceptance remains tracked in [#9](https://github.com/CivicPulse/updaters/issues/9).
The operator is **Kerry Hatcher**; actionable email alerts go only to
**kerry@kerryhatcher.com**. Production collection stays disabled until the reviewed
acceptance record is complete. A healthy process, merged PR, public object, or
successful email test alone does not establish acceptance.

## Identity, host and window

Use Kubernetes context `do-atl1-k8s-1-37-0-do-2-atl1-1793724241236` for all workloads:
`civpulse-dev`, `civpulse-test`, and `civpulse-prod`. Images come from
`registry.digitalocean.com/hatchk8s/civpulse-results-collector` by immutable digest.
Dev/test use separate self-hosted Supabase releases and R2 credentials/buckets;
they must never receive the production service-role key.

The existing public production election is
`295d864d-80c8-4a87-ab16-3f4f3fa5ddb7`, date `2026-11-03`. Its approved exact source is:

```text
https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json
```

The source URL was NULL on the initial record read. The authorized admin save,
current public visibility, date, durable feed configuration and reviewed counts
must be verified before activation; this runbook does not perform that save.
Never create a substitute production election or infer identity from names.

After acceptance, the actual activation must request an **immediate check** through
the existing authenticated Supabase coordinator. Source checks are hourly until
`2026-11-03T22:00:00Z` (November 3, 5 PM Eastern), then every five minutes. If
activation finishes after the transition, start at the five-minute cadence. Stop
at `2026-11-11T00:00:00Z` (November 10, 7 PM Eastern), and review before extending.
Record the actual activation and first real source-check timestamps in issue #9;
never substitute the planned start or deployment timestamp.

One Supabase coordinator owns source scheduling. Its cron can tick every five
minutes while database eligibility gates the effective hourly/five-minute source
cadence. Do not add a Kubernetes source CronJob, independent Edge scheduler, manual
table writes, or per-batch build webhook. The separate one-minute monitoring
CronJob reads health/S3 and sends operational notifications; it never claims jobs,
checks the SOS source, stages data, changes feed configuration, or triggers builds.

## Evidence before activation

Copy [the acceptance template](results-acceptance.template.json) into the reviewed
release evidence location. Keep `accepted:false` until each required check links
to actual results, including reviewed reference commits, compatible Rust
fingerprints/assets, official and enlarged-feed measurements, selected host/cost,
and main-first migration/Edge Function deployment.

Exercise a controlled import in isolated dev/test: exact raw checksum, complete
staging, immutable scoped assets, DB activation, conditional pointer publication,
recovery after stage/commit/ack crashes, downward corrections and timestamp-only
replays. Verify the browser's already-open ballot candidate/choice cards, source
attribution, missing counts, detail assets, mappings and public/private gates.
Keep the source receipt and expected completeness counts with the evidence.

Verify production secret isolation and actual S3 conditional-write/checksum
behavior using approved public source evidence and the established contract;
synthetic test fixtures do not become production election evidence. Verify real
`files.civpulse.org` CORS/cache behavior and the configured alert route. Mark
unperformed checks pending. Do not claim production source import/browser
readback before it has happened. After approved activation, verify the immediate
first import and public readback promptly; pause on failure and retain issue #9
until its acceptance criteria are actually satisfied.

Production schema changes must first reach the website repository's `main` and
use its guarded approved migration path. Never apply feature-branch SQL, repair
migration history, use production execute-SQL to bypass the guard, or invent
collector-owned tables to work around a missing coordinator operation.

## Read-only observation and notifications

`health` reads the service operational RPC. `monitor` emits sanitized JSON and
returns nonzero for actionable conditions. `metrics` emits Prometheus text with
exact revisions kept as strings in JSON rather than lossy float gauges. Both
read the discovery pointer through the consistent signed S3 API, never a cached
public CDN response. They perform remote reads only; their local observation file
contains UUIDs and timestamps, not raw source payloads, error bodies or secrets.

`notify` collects the same observation, sends only a new/changed actionable alert
set or one recovery, and persists its delivery deduplication state. Healthy or
unchanged reports stay quiet. A failed operational RPC becomes a deployment-level
monitoring-unavailable alert; the sender does not require a healthy database to
report that failure. Mail transport failures return nonzero without recording a
successful send. A crash after provider acceptance but before local persistence
can duplicate a notification; the protocol favors retry over silently losing it.

The monitor uses `source_checks_expected`, effective `poll_interval_seconds`, and
`source_check_expected_since` from the coordinator. It respects paused/future/ended
windows and gives activation/cadence transitions a real check opportunity without
forging `last_attempt_at`. Hourly check staleness is **125 minutes**; five-minute
check staleness is **15 minutes** (`max(900, 2 * interval + 300)` seconds). Raw last
attempt/success timestamps remain in JSON. A pending publication backlog (from actual job creation when available, otherwise
first observed pending) or DB/pointer mismatch sustained for 15 minutes, repeated errors (at least three attempts), quarantine
(eight attempts), or unavailable S3 pointer is actionable. An unchanging vote count
is never an alert signal.

The chart's optional monitoring CronJob uses `concurrencyPolicy:Forbid`, no job
retries, a bounded deadline, a non-root/read-only container, namespace-private PVC
for observation/dedup state, process file locks, and no inbound port or service
account token. The PVC uses the retaining DOKS block-storage class and Helm's
`keep` policy. Do not delete it during routine upgrades or rollbacks. Network
policy permits DNS, public HTTPS and (dev/test only) namespace-local Supabase Kong.
Standard Kubernetes NetworkPolicy cannot restrict public HTTPS by hostname; the
code fixes R2, Supabase and email destinations and rejects redirects.

Optional [Prometheus rules](../../deploy/monitoring/results-alerts.yaml) and
[timing tests](../../deploy/monitoring/results-alerts.test.yaml) are supplied.
Wire them into an existing scrape/textfile bridge if used; they are not evidence
that a Prometheus/Alertmanager installation exists. Direct notification works
without that stack. The monitor CronJob itself stopping still needs the host's
job/monitor availability route or a configured external scrape alert.

## Alert credentials and delivery verification

Cloudflare account `6d837580a4d0641139ecada9e74076b8` owns the isolated verified
sending domain `alerts.civpulse.org`; sender is fixed to
`results@alerts.civpulse.org` and recipient to `kerry@kerryhatcher.com`. Only new
bounce/SPF/DKIM/DMARC records under that subdomain were provisioned. Existing main
domain MX/SPF records are unchanged.

`results-alert-email` in each environment contains only
`CLOUDFLARE_EMAIL_API_TOKEN`. The explicitly approved account-scoped Email Sending
Write token expires `2026-11-11T01:00:00Z`; the provider does not expose a narrower
send-only permission. No account administration, DNS, bucket deletion or database
privileges belong to this token. Treat it as a secret: never print it, embed it in
Helm values, put it in Git, or include it in logs. Renew/rotate through a reviewed
operator action before any approved extension.

Notifications require explicit `SOS_ALERTS_ENABLED=true`; default is false. PVC
paths are `SOS_MONITOR_STATE=/state/observations.json` and
`SOS_ALERT_STATE=/state/alerts.json`. If an older operational RPC is temporarily
present, fallback configuration is `SOS_POLL_INITIAL_INTERVAL_SECONDS=3600`,
`SOS_POLL_TRANSITION_AT=2026-11-03T22:00:00Z`, `SOS_POLL_INTERVAL_SECONDS=300`.
This fallback changes monitoring expectations only, never scheduling.

After the reviewed image is deployed, use
`scripts/results-alert-test.py --environment test --image-digest sha256:<reviewed-digest>`
to render a one-shot Job, then add `--apply` for exactly one controlled `alert-test`
command with the test namespace's email Secret and alerts enabled. It needs no
Supabase/source/R2 credentials and cannot enable collection. Record the returned
Cloudflare message ID and `delivered` status. `queued` means accepted for deferred
delivery and is not a delivery proof; inspect provider delivery evidence before
marking acceptance. A previous connector email from an unrelated sender proves
recipient connectivity only and does not satisfy deployed CivicPulse delivery.
Each applied verification creates a fresh Job and records its name and UID; the
wait and logs refer to that same Job. Record operator inbox acknowledgement
separately: even a provider `delivered` receipt does not establish that a person
received/read the message.

## Pause and resume

Use the existing admin RPC through the helper. It defaults to a printed plan;
`--apply` executes with `CIVPULSE_ENV`, `SUPABASE_URL`, `SUPABASE_ANON_KEY` and an
administrator `SUPABASE_ACCESS_TOKEN` injected from the secret store. Service-role
credentials cannot replace the administrator session. HTTPS is required outside
explicit dev/test loopback. Secrets enter curl through stdin, never process args.

```sh
python3 scripts/results-admin.py --environment prod --action pause \
  --election-id 295d864d-80c8-4a87-ab16-3f4f3fa5ddb7
```

Inspect the plan, then execute the same command with `--apply`. Pausing through
`configure_sos_results_feed(p_enabled:false)` stops coordinator source checks while
retaining current results/evidence. Stopping the worker alone does not pause
checks. For an incident, pause first. Current claim and lease guards require an enabled feed, so pausing also fences queued or active work; preserve its receipt and use a deliberate controlled resume for recovery. Stop the worker and
wait for its fenced lease to expire. Do not kill a renewal and immediately assume
an ambiguous remote write was undone.

Resume requires the exact source URL and explicit start/end window. Production
also requires a completed matching acceptance record. The helper refuses to
re-activate an already enabled feed; pause explicitly first. It then verifies the
paused state and
checks the stored source URL, configures the paused feed through
`configure_sos_results_poll_cadence` (initial 3600 seconds, transition 22:00Z, then
300 seconds), then enables through the guarded admin operation;
a mismatch leaves the feed paused. Before activation, `--action window` uses the paused-only
`configure_sos_results_feed_window` helper to save explicit start/end timestamps
without enabling collection. Resume verifies that persisted paused window before
the final activation call. A missing window RPC leaves the feed paused.

The production helper refuses a different cadence or a stop-time extension.
The approved cadence migration must already be deployed; an unsupported RPC leaves
the feed paused. Activation uses the existing coordinator and requests its
immediate authenticated check.

## Replay, quarantine and pointer repair

Read `monitor` and sanitized job logs, identify the cause, and fix credentials,
network access, schema expectations or immutable assets first. Wait for lease
expiry. `retry` and `pointer-repair` both call
`recover_sos_results_job(p_action:"retry")`; they reset attempts and preserve the
original raw receipt/staging/outbox. A subsequent fenced worker resumes that
exact source even if upstream has advanced.

```sh
python3 scripts/results-admin.py --environment prod --action pointer-repair \
  --election-id 295d864d-80c8-4a87-ab16-3f4f3fa5ddb7
```

Plan before `--apply`. For an invalid **uncommitted** job, the deliberate `abandon`
action ends it so a later approved poll may enqueue a new source. The database
rejects abandonment of a committed pointer; repair that outbox and retry. Never
unconditionally overwrite `latest.json`, copy an older pointer, delete staging
rows, change a receipt to a newer download, or bypass a stale lease with table SQL.
A pointer-phase replay skips source download/import and can acknowledge an already
written matching revision after a crash.

## Cutover and rollback

This is the initial Rust deployment. The reference Bun collector was never deployed;
there is no reference deployment to drain or stop and no existing reference image
rollback target. Deploy Rust disabled, verify compatibility and recovery in dev/test,
then complete the approved production acceptance gates. Record the actual initial
activation in issue #9. One active collector implementation owns collection.

For rollback, pause the feed through the admin helper first, then drain/stop Rust.
Redeploy a reviewed previous compatible image **with collection disabled** and the
same environment secrets; preserve the monitor PVC. Do not blindly `helm rollback`
an old release that may restore `collection.enabled:true`. Verify pointer/DB
consistency, then separately review any resume. Rollback does not erase snapshots,
restore an older public pointer, or apply reverse SQL migrations.

## Evidence retention and incident record

Retain published raw files, immutable manifests/projections and database snapshots
indefinitely. Retain recoverable/abandoned staging initially; after 30 days review
unreferenced objects while paused against every current pointer, snapshot and job
receipt before proposing a separate deletion. No blanket lifecycle deletion is
configured. Scratch directories are expendable only after their owning parser has
stopped. Visibility changes control future admission and gated database reads;
already public files and uploads in flight cannot be recalled.

In issue #9 record release/image/chart digests, website migration/Edge versions,
resource/cost evidence, operator, exact identity/URL/counts/window, alert delivery
proof, recovery drills, actual activation/immediate check, first publication and
already-open browser readback. Leave missing proofs pending and keep the parent
feature open until the complete acceptance criteria are met.

## Resource acceptance

Run `kubectl --context k8s-1-37-0-do-2-atl1-1793724241236 -n civpulse-test exec <collector-pod> -- /usr/local/bin/civpulse-tool resources` during and after each controlled single-job run. The command reads only its own container cgroup and fixed spool directories. JSON null means unsupported, not zero. Cgroup current/peak include page cache; process RSS measures the command itself. Record anon/file/shmem, OOM counters, CPU usage/throttling, logical and allocated scratch, pod restart count, elapsed runtime, and node capacity/headroom alongside both dev/test databases. Cgroup peak is lifetime peak, so use a fresh pod per case. Resource measurement does not need Metrics Server or a shell.

The 174 MB uniform fixture alone does not prove the configured 512 MiB raw / 64 MiB scope envelope. Before activation, exercise a near-cap raw case with concentrated near-cap scope and archive replay, recording rejection behavior and successful cleanup. Alternatively select lower admission limits only after equivalent near-selected-cap tests. `collection.maxDownloadBytes` / `collection.maxScopeBytes` map to `SOS_MAX_DOWNLOAD_BYTES` / `SOS_MAX_SCOPE_BYTES`; both must be positive and can only lower the fixed caps, with scope no larger than raw. Defaults retain the full contract. Candidate 128 MiB / 16 MiB limits are not an accepted resource envelope until measured. Download and archived replay enforce these limits before parsing; oversized feeds fail closed and require an operator capacity review rather than silent truncation or automatic limit increases. Record the exact selected limits and evidence in acceptance.

Cost evidence must include block storage separately from the node baseline: dev/test each allocate 10 GiB database + 1 GiB pgsodium + 1 GiB storage PVCs, 24 GiB total before monitor PVCs. These volumes are billable; use the actual provider rate/invoice rather than treating them as free or asserting an unverified rate.

### Reproducible concentrated resource cases

Generate each case outside the cluster, one jurisdiction at a time. The generator preserves all original IDs, 160 jurisdictions and 4,178 items, adds explicit synthetic ASCII padding in one appended precinct-detail row per item (unique ID, null vote count, existing rows/fields unchanged), validates output with the real parser, and prints exact bytes/hash. It creates a new output only. Full-cap candidate: `cargo run --release -p civpulse-results-collector --example resource_case -- OFFICIAL.json resource-fixture.json 524288000 62914560` (500 MiB raw / 60 MiB county). Lower-cap candidate: use `125829120 15728640` (120 MiB raw / 15 MiB county) for selected 128/16 MiB admission. Precinct detail is omitted from compact assets; the 4 MiB compact-asset limit remains unchanged. An impossible target or unsafe per-item size fails instead of silently changing counts or overflowing a scope. These are synthetic resource cases, never public election results.

The scanned collector image includes `/usr/local/bin/civpulse-measure` as an auxiliary binary; the production daemon entrypoint remains unchanged. Other legacy tool images do not contain the auxiliary binary. Do not substitute another harness image for actual digest evidence. With the test collector daemon disabled and no running job, use `python3 scripts/results-resource-job.py --pod EXACT-FRESH-TEST-POD --image-digest sha256:DIGEST --fixture resource-fixture.json` for a dry run, then `--apply` only for the controlled resource case. The wrapper verifies the actual running collector image and requires a fresh, ready, zero-restart test pod. It streams the fixture through `kubectl exec -i` directly to the auxiliary binary, without a shell, tar, credentials in arguments, new source scheduler or separate harness image. Input is bounded by the deployment admission cap and stored in a temporary file with automatic cleanup. The real Publisher uses only loopback fenced RPC and local disk object-store adapters; it never loads the pod's Supabase/R2 credentials or performs external writes. The daemon stays disabled throughout.

The auxiliary binary runs the real parser and Publisher through original archived-receipt replay, staging, commit and pointer confirmation. Its simulated RPC/object storage, input copy, and preliminary validation add conservative memory/scratch overhead; cgroup measurements include the idle daemon in the same deployed container. This supplies actual-image resource evidence, not real DB/R2 transport/CAS acceptance. The actual collector `once` run against dev/test DB/R2 on official data is a separate required gate.

Record the wrapper output, cgroup counters, pod status/restarts and node capacity/headroom evidence while both selfhost databases run. Preserve failed/OOM pod evidence. Use a fresh reviewed disabled collector pod for each case to reset lifetime cgroup peaks; preserve evidence before replacing it. Record exact fixture hash and caps from the generator. Test official data, concentrated near-admission input and original receipt replay (`--pipeline`), plus over-cap rejection, before selecting limits. Never invoke this harness on production pods. The resulting measurements are not production collection acceptance by themselves.
