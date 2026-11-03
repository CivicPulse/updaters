# CivPulse updaters

Cargo workspace for independently deployed Rust batch tools that download,
process, and upload CivPulse data. These are starter commands: no source,
transformation, API, credentials, or pipeline orchestration is wired yet.

## Layout

- `tools/download`: `civpulse-download` executable; future source-specific downloads.
- `tools/process`: `civpulse-process` executable; future parsing and normalization.
- `tools/upload`: `civpulse-upload` executable; future CivPulse API uploads.
- `crates/civpulse-runtime`: JSON logging, batch lifecycle, and SIGTERM/Ctrl-C handling.
- `deploy/k8s`: one suspended Job template per executable.

Only executable crates get images and Kubernetes workloads. Shared library crates
are linked into those binaries. Add source-specific tools under `tools/` and
shared libraries under `crates/`; workspace glob membership picks them up.
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
and logging only, returns zero, and performs no data operations. No arguments or
unsupported arguments return nonzero so unfinished jobs cannot report successful
data ingestion. `RUST_LOG` controls filtering (default `info`); logs go to stderr
as JSON. Invalid logging filters fail startup.

## Containers and Kubernetes

Build each executable separately from the repository root:

```sh
docker build --build-arg TOOL=civpulse-download -t civpulse-download:dev .
docker build --build-arg TOOL=civpulse-process -t civpulse-process:dev .
docker build --build-arg TOOL=civpulse-upload -t civpulse-upload:dev .
docker run --rm --read-only civpulse-download:dev --check
```

The same Dockerfile builds one selected binary per image. Images include CA
certificates for future HTTPS clients and run as an unprivileged numeric user.
The runtime is a digest-pinned distroless Debian 13 image with no shell or package
manager; probes and diagnostics must invoke the executable directly.
CI builds and checks all three images without pushing them.

Job manifests are examples, suspended by default, with `--check` arguments. Set
an image available to your cluster (prefer a digest), namespace, resource sizing,
and any needed configuration before use. For a scaffold smoke check, load/push
the image, apply the selected manifest and explicitly unsuspend its Job. No
resources are deployed automatically by this repository. These Jobs do not
schedule or trigger each other; add CronJobs or workflow orchestration when the
actual sources and dependencies are known. Job pod exit status records completion;
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

Before enabling data operations, add source-specific configuration validation,
provenance contracts, bounded network timeouts, and tests at the real I/O boundary.
There are no database/API adapters or production deployment assumptions yet.

## Georgia SOS durable worker

`tools/results-collector` adds the queue/lease lifecycle for issue #1. Its source
and publication adapters are deliberately unavailable in this implementation
step: an enabled worker receiving a job records a bounded durable failure and
exits nonzero in `once` mode. It never reports an unimplemented import as success.
Production collection must remain disabled until the complete pipeline and
controlled acceptance pass.

```sh
cargo run --locked -p civpulse-results-collector -- --check
cargo run --locked -p civpulse-results-collector -- daemon
cargo run --locked -p civpulse-results-collector -- liveness
# Server-side credentials required; reads operational state without claiming work:
cargo run --locked -p civpulse-results-collector -- health
```

Commands are `daemon` (the default), `once`, `health`, `liveness`, `validate`,
`--check`, and `--help`. `--check` performs no I/O. `validate` returns an explicit
unavailable error until the bounded parser is implemented. `once` claims at most
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
| `SUPABASE_URL` | Required for enabled collection and dependency health; HTTPS except explicit loopback development services |
| `SUPABASE_SERVICE_ROLE_KEY` | Server-only secret; never logged |
| `RUST_LOG` | `info`; JSON logs go to stderr |

A disabled daemon stays alive, writes a local heartbeat, and performs **no RPC
calls**, even with credentials present. Feed pause/resume remains the existing
admin `configure_sos_results_feed` operation; stopping a worker does not disable
the Supabase source schedule. Queue checks every 15 seconds are separate from
the existing authenticated five-minute SOS polling schedule.

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
