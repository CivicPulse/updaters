# CivPulse updaters

Cargo workspace for independently deployed Rust batch tools and a durable Georgia
SOS election results worker. The generic download/process/upload commands remain
scaffolds; the results collector implements bounded validation, fenced Supabase
jobs, immutable R2 evidence/assets, and discovery-pointer publication. Production
collection stays disabled until controlled operational acceptance completes.

## Layout

- `tools/download`: `civpulse-download` executable; future source-specific downloads.
- `tools/process`: `civpulse-process` executable; future parsing and normalization.
- `tools/mcp`: standalone 79-tool V2 MCP service; see [configuration and production deployment](tools/mcp/README.md).
- `tools/upload`: `civpulse-upload` executable; future CivPulse API uploads.
- `tools/results-collector`: durable queue consumer, local validation, and atomic publication.
- `crates/civpulse-results-source`: bounded downloading, parsing and reference-compatible fingerprints.
- `crates/civpulse-results-contract`: public asset contracts and pinned golden fixtures.
- `crates/civpulse-webapi`: authenticated REST service.
- `crates/civpulse-runtime`: JSON logging, batch lifecycle, and SIGTERM/Ctrl-C handling.
- `deploy/k8s`: one suspended Job template per generic scaffold executable.
- `deploy/helm/civpulse-results-collector`: disabled-by-default durable worker chart.
- `deploy/environments`: dev/test values targeting Thor and production values targeting DigitalOcean.
- `deploy/supabase`: pinned, isolated dev/test database bootstrap and local replay evidence.

Only executable crates get images and Kubernetes workloads. Shared library crates
are linked into those binaries. Add source-specific tools under `tools/` and
shared libraries under `crates/`; workspace glob membership picks them up.
HTTP services deploy independently and share an environment router/ingress and
FQDN through distinct path prefixes; see [service deployment architecture](docs/architecture.md).
The queue-driven results worker needs no public route.

Dependency versions, metadata, and lint policy live in the root `Cargo.toml`.
Workspace inheritance follows the [Cargo workspace reference](https://doc.rust-lang.org/cargo/reference/workspaces.html).

## Election results collector

The production collector is being implemented under [issue #1](https://github.com/CivicPulse/updaters/issues/1).
The pinned reference, RPC/public-file contracts, synthetic golden fixtures, and
activation gates are documented in [the compatibility contract](docs/contracts/results-collector.md).
Collection remains disabled until controlled acceptance is complete.

## Development

Install Rust using rustup; `rust-toolchain.toml` pins the build toolchain.
Commit the shared `Cargo.lock` when dependencies change.

```sh
cargo build --locked --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p civpulse-download -- --check
cargo run --locked -p civpulse-process -- --help
```

Each executable accepts `--help` and `--check`. The latter checks process startup
and logging only, returns zero, and performs no data operations. The three generic starter tools return nonzero for missing or
unsupported arguments so unfinished jobs cannot report successful ingestion.
The results collector defaults to a disabled daemon; its modes are described below. `RUST_LOG` controls filtering (default `info`); logs go to stderr
as JSON. Invalid logging filters fail startup.

## Containers and Kubernetes

Development and test run on Thor; production runs on DigitalOcean Kubernetes.
See the [hosting and environment overview](deploy/README.md) for contexts,
namespaces, routing, storage, capacity, and rollback procedures.

Build each executable separately from the repository root:

```sh
docker build --build-arg TOOL=civpulse-download -t civpulse-download:dev .
docker build --build-arg TOOL=civpulse-process -t civpulse-process:dev .
docker build --build-arg TOOL=civpulse-upload -t civpulse-upload:dev .
docker build --build-arg TOOL=civpulse-results-collector -t civpulse-results-collector:dev .
docker run --rm --read-only civpulse-download:dev --check
```

The same Dockerfile builds one selected binary per image. Images include CA
certificates for future HTTPS clients and run as an unprivileged numeric user.
The runtime is a digest-pinned distroless Debian 13 image with no shell or package
manager; probes and diagnostics must invoke the executable directly.
Pull-request CI builds and checks all six images, including the real disabled
collector entry point, heartbeat, scratch permissions and bounded SIGTERM. A
separate workflow builds, checks, scans, and publishes all six images on every
push to `main` (including merged pull requests). It can also be run manually from
`main`. Images are published as
`registry.digitalocean.com/hatchk8s/<tool>:sha-<full-commit-sha>`, and each job
records its immutable digest reference in the run summary. MCP uses a distinct
`mcp-sha-<full-commit-sha>` tag in the existing `civpulse-webapi` repository because
the registry permits five repositories. Publishing deploys nothing.

Before the first publish, configure the repository Actions secret `DOCR_CONFIG`
with a Docker authentication JSON object containing push-capable credentials for
`registry.digitalocean.com` (the contents of a Docker `config.json`, not a
base64-encoded Kubernetes Secret). Credentials must have access to the `hatchk8s`
registry. The workflow loads credentials only for the push step into a temporary
directory and removes it afterward. Use the emitted image digest for deployment.

Job manifests are examples, suspended by default, with `--check` arguments. Set
an image available to your cluster (prefer a digest), namespace, resource sizing,
and any needed configuration before use. For a scaffold smoke check, load/push
the image, apply the selected manifest and explicitly unsuspend its Job. No
resources are deployed automatically by this repository. These starter Jobs do not
schedule or trigger each other. The results collector consumes the existing
Supabase durable queue; Supabase owns the single five-minute Cron heartbeat and
database-gated source cadence/window. Job pod exit status records completion;
batch tools do not need an HTTP health server.

Each pod has a read-only root filesystem and bounded ephemeral `/tmp`. Files there
are lost with the pod; stages must exchange artifacts through explicitly configured
object storage or persistent volumes. Keep original source bytes and provenance;
normalize only derived copies. Inject future credentials through Secret references,
never image layers or committed manifests. Do not log credentials or raw private data.

## Implementing tools

Replace the call to `scaffold` with argument/configuration parsing and an operation
passed to `civpulse_runtime::run`. Keep business logic in the tool crate; extract
shared models, provenance, HTTP clients, or storage adapters only when there is a
concrete shared contract. Avoid dependencies between executable crates.

The runtime drops the operation future on shutdown and returns nonzero. Future
operations must be cancellation safe, checkpoint externally, and reconcile remote
writes whose outcomes are unknown. Do not wrap blocking work in async code and
assume dropping its future stops it. Build upload idempotency before enabling
retries: example Jobs use `backoffLimit: 0` and a one-hour deadline. Tune that
policy for each implemented tool. Libraries should return errors; executable exit
status must reflect incomplete work.

The generic starter commands still need source-specific adapters before data
operations can be enabled. The results collector has its own validation,
provenance, bounded I/O, and recovery contracts documented below.

## Georgia SOS durable worker

`tools/results-collector` implements the queue/lease lifecycle, bounded source
validation, resumable staging, immutable evidence/assets, and conditional discovery
publication for issue #1. Enabled workers construct the publication adapter only
from explicitly reviewed environment configuration. Production collection must
remain disabled until controlled acceptance passes; implementing the pipeline
and isolated R2 tests does not establish production readiness.

See [publication configuration and recovery](docs/results-publication.md) for
required identity/completeness settings and environment-scoped R2 credentials.
Dev/test/prod workloads target `k8s-1-37-0-do-2-atl1-1793724241236`, isolated in
`civpulse-dev`, `civpulse-test`, and `civpulse-prod` namespaces. Dev/test buckets
and origins cannot be substituted with the production pair.

```sh
cargo run --locked -p civpulse-results-collector -- --check
cargo run --locked -p civpulse-results-collector -- daemon
cargo run --locked -p civpulse-results-collector -- liveness
# Local validation without credentials or remote writes:
cargo run --locked -p civpulse-results-collector -- validate /path/to/source.json 2026-11-03 160 4178
# Server-side credentials required; reads operational state without claiming work:
cargo run --locked -p civpulse-results-collector -- health
```

Commands are `daemon` (the default), `once`, `health`, `liveness`, `validate`,
`--check`, and `--help`. `--check` performs no I/O. `validate` reads a local regular
file and checks its date and optional completeness counts. Its parser and
fingerprints match the [pinned reference](docs/contracts/results-source-api.md).
`once` claims at most
one durable job; an empty queue is a normal successful check. `health` calls the
service-only `get_sos_collection_health` RPC without requiring R2 credentials or
an enabled collector. It checks connectivity/access, not whether source counts
have changed. `liveness` checks only a recent local heartbeat, so a Supabase
outage must not trigger pod restart loops.

Configuration:

| Variable | Default / purpose |
| --- | --- |
| `SOS_COLLECTOR_ENABLED` | `false`; only the exact strings `true` and `false` are accepted |
| `SOS_COLLECTOR_CONCURRENCY` | `1`, restricted to `1..=4` |
| `SOS_COLLECTOR_HEARTBEAT` | `/tmp/civpulse-results-heartbeat` |
| `CIVPULSE_ENV` | `prod`; exact `dev`, `test`, or `prod` environment boundary |
| `SUPABASE_URL` | Required for enabled collection and dependency health; production HTTPS, or the exact matching dev/test namespace gateway HTTP origin |
| `SUPABASE_SERVICE_ROLE_KEY` | Server-only secret; never logged |
| `RUST_LOG` | `info`; JSON logs go to stderr |

A disabled daemon stays alive, writes a local heartbeat, and performs **no RPC
calls**, even with credentials present. Feed pause/resume remains the existing
admin `configure_sos_results_feed` operation; stopping a worker does not disable
the Supabase source schedule. Queue checks every 15 seconds are separate from
the existing authenticated Supabase Cron heartbeat and configured source cadence.

The worker renews five-minute fenced leases every 30 seconds independently of
asynchronous processing. Lease loss cancels processing and prevents subsequent
mutations through the fenced interface. SIGTERM/Ctrl-C stops new claims, drains
existing work for at most 90 seconds while renewing leases, and cancels remaining
work after that deadline. Deployment termination grace must exceed this drain
plus process shutdown overhead. Durable Postgres/R2 receipts own recovery; local
scratch and heartbeats are disposable. Processor implementations must be
cancellation-safe and must observe the lease cancellation receiver during
blocking parsing; dropping an async future does not stop an uncontrolled thread.

RPC requests reject redirects, set both `apikey` and bearer headers, have a
60-second total deadline, and accept at most 4 MiB of response bytes. Errors are
classified without exposing URLs, headers, database bodies, or dependency error
chains. Durable retry timing and quarantine remain owned by the database.

Offline real/enlarged resource measurements and acceptance evidence are documented
in [results verification](docs/results-verification.md). These local simulations
do not authorize production collection.

## Disabled Helm deployment

Use the [collector chart](deploy/helm/civpulse-results-collector/README.md) on
`do-atl1-k8s-1-37-0-do-2-atl1-1793724241236` with namespaces `civpulse-dev`,
`civpulse-test`, and `civpulse-prod`. Supply the exact scanned manifest digest; all
committed environment values disable collection. Production activation requires
recorded acceptance in addition to the database feed controls. The provisional
512Mi request / 1Gi memory limit / 2Gi scratch envelope still requires actual
cgroup, worst-case asset/replay and shared-node capacity acceptance.

The [self-hosted Supabase bootstrap](deploy/supabase/README.md) is limited to
dev/test, checks the merged website migration lock, and creates no external
poll/rebuild Vault target or enabled feed. Local SQL replay evidence does not
claim a Helm rollout or production acceptance.

Operational acceptance, alerting and recovery: [results runbook](docs/operations/results-runbook.md).

## REST API

Run `cargo run --locked -p civpulse-webapi` with `SUPABASE_URL` and
`SUPABASE_PUBLISHABLE_KEY` set. `API_BIND` defaults to `0.0.0.0:8080`.
`--check` is an offline binary startup check and does not validate configuration.

Every HTTP request requires `Authorization: Bearer <user access token>`.
Tokens are validated with Supabase Auth on every request; anonymous sign-ins are
rejected. No user-editable metadata or proxy identity headers grant permissions.
Database requests use the caller's token, preserving existing grants and RLS,
including the website's database-backed admin allowlist. No privileged backend
key is used. Auth/data outages fail closed; credentials and backend error bodies
are not logged or returned. API responses disable caching.

Initial routes:
- `GET /api/v1/me`: verified user ID.
- `GET /api/v1/organizations`: at most 100 visible organizations, ordered by ID,
  with id, slug and name. This is an initial bounded listing, without pagination
  or writes. Future operations must retain caller identity and database policies.

Production routing for `webapi.civpulse.org` uses an outbound Cloudflare Tunnel
and internal Traefik services, with no DigitalOcean load balancer. See
[deployment instructions](deploy/edge/README.md) for secrets, DNS, rollout,
permission checks, and adding future microservices.

## Civic Pulse MCP

The production MCP service shares the API's DigitalOcean cluster, namespace and
Traefik router at `https://webapi.civpulse.org/mcp/v2`. The API retains `/api/v1`.
See [the MCP deployment and rollback guide](tools/mcp/README.md) and
[release receipt](deploy/k8s/mcp-release.json).
