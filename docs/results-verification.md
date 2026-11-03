# Results collector verification and resource envelope

This unit verifies the Rust implementation locally. Production collection stays
disabled until the separate deployment/readback acceptance is recorded. The
local benchmark does not establish production RPC permissions, R2 semantics,
public caching, browser updates, or an operating alert route.

## Repeatable offline measurements

Use Linux, the pinned Rust toolchain, and a local copy of the reviewed export.
No credentials are required. The example opens only a loopback RPC server and
uses a local filesystem object-store double. It never downloads from SOS, writes
to Supabase, or uploads to R2. It consumes a job containing an archived-original
receipt, so the real `Publisher` exercises restart recovery through the original
raw bytes rather than replacing them with a later upstream response.

```sh
mkdir -p /path/on/local/disk/results-scratch
export CIVPULSE_MEASURE_SCRATCH_ROOT=/path/on/local/disk/results-scratch
scripts/measure/make-large.sh /path/ga-20261103.json /path/ga-large.json
scripts/measure/results.sh /path/ga-20261103.json 2026-11-03 /path/real --pipeline
scripts/measure/results.sh /path/ga-large.json 2026-11-03 /path/large --pipeline
```

The measurement example refuses tmpfs/ramfs scratch. `stat -f` reports the
filesystem type in its JSON output; Linux may report an ext4 mount as
`ext2/ext3`. A 10 ms sampler records logical file sizes separately for worker
scratch and the simulated remote-object directory. The source input is excluded.
Sampling can miss brief peaks and directory metadata is excluded. The simulator
retains raw/immutable objects locally, whereas production keeps those objects in
R2; size the pod from **worker scratch**, not total simulator disk usage.

`results.sh` requires Python 3 and writes a JSON report plus a GNU
`/usr/bin/time -v` CPU/wall/RSS report. Choose a fresh output prefix: either existing
report path is rejected before build/measurement, including source aliases, symlinks
and hard links. Output files are reserved create-only with exclusive descriptors;
GNU time writes through its reserved descriptor rather than reopening a source
pathname. Failed runs remove only their own incomplete reports. Do not read a
report until the command succeeds. Existing source and report evidence is never
overwritten by this wrapper. The Rust harness and recorded measurement receipts
are unchanged by this report-publication safeguard.
Build time is excluded. Total measured time includes an initial exact-byte hash
and validation, followed by a second validation inside the simulated pipeline.
The pipeline measures streaming archive copies/hashes, original-receipt replay,
bounded staging serialization, immutable asset generation/checksum readbacks,
the database publication interface, pointer conditional write and readback.
The local RPC double counts/discards bounded batches; it does not simulate
Postgres query cost, remote latency, concurrent production writers, or admission
rules. Peak RSS is a process high-water mark, not complete cgroup memory usage;
filesystem page cache and host/container overhead need deployed validation.

The larger generator holds one jurisdiction at a time and refuses an existing
output path, preserving source evidence on accidental path reuse. It preserves the real
source and adds a synthetic 32 KiB `resourceTestPadding` string to each of its
3,904 county ballot items. This changes resource size, not the election evidence.
Its exact bytes match the website reference fixture:

| Input | Bytes | Exact-byte SHA-256 |
| --- | ---: | --- |
| November receipt | 46,363,141 | `f464d3b037a82d594c893aef072042530a84b6b1cb3edb76b1e3873bed2dc3ba` |
| Larger fixture | 174,387,013 | `4f4ce3c4856a261096848f7b1f033e69ab4e1501f61099411ea771a12a89d2c9` |

Both inputs contain 160 jurisdictions and 4,178 items. The real source reports
`2026-11-03T10:28:19.3118441Z`; its Rust semantic fingerprint is
`08145568d3e4ca710d32f831d71688205bd63ed1365dd2bdaff619e1e9c99bbb`.
The padded fingerprint is
`57b28cf5432925af8ba78a93be57c31d7bd56b3c937b148f0ed59eef2e27fee2`.

## Recorded local measurements

Recorded November 3, 2026 on Linux x86_64, AMD Ryzen 7 5800X, Rust 1.99.1,
release profile, ICU 2.1 pinned closure, two Tokio worker threads, ext4 scratch.
Runs are sequential with warm input caches. These are individual local runs;
background host activity was not controlled and they are not latency guarantees.
Machine-readable measurements and GNU time reports are in `docs/measurements/`.
The reports identify Publisher/source code base
`32db87fb33eab3b69770b2807dd8763ef46390af` and include the SHA-256 of the
measurement example plus its disk/RPC support modules. The harness is included
in this change; all source/publication code measured comes from that reviewed
base revision.

| Input | Validation | Simulated pipeline | Total wall | User / system CPU | Peak RSS | Worker scratch peak | Total local peak |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| real | 3.846 s | 7.120 s | 11.020 s | 10.33 / 0.91 s | 328.3 MiB | 88.4 MiB | 179.4 MiB |
| large | 9.883 s | 13.846 s | 23.896 s | 22.63 / 1.78 s | 403.6 MiB | 332.6 MiB | 789.9 MiB |

Both pipelines staged all 4,178 items in 173/178 batches respectively. Largest
RPC envelopes were 2,087,943 / 2,090,138 bytes, below the 2 MiB item-batch
envelope plus bounded RPC metadata. Readback verified revision `9007199254740993`.

## Selected host and initial deployment envelope

The selected Kubernetes cluster is
`k8s-1-37-0-do-2-atl1-1793724241236`, with separate `civpulse-dev`,
`civpulse-test`, and `civpulse-prod` namespaces. The existing node pool has three
`s5-2vcpu-2gb-50gb` nodes, approximately 1.4 GiB allocatable memory per node.
At the inspected node price of $34.65/month, existing compute is $103.95/month
for three nodes, before additional disks, storage, transfer, or other charges.
This is an inventory/price observation, not a new host purchase or full cost
forecast. Recheck actual billing and capacity before activation.

Start with one job per worker, 512 MiB requested memory, a 1 GiB memory limit,
one CPU limit, and 2 GiB bounded disk-backed temporary storage. These are
provisional values for deployed acceptance on the selected small nodes, justified
only for the real and enlarged fixtures measured here. Three environments share
the cluster: verify available node capacity, scheduling, eviction behavior and
existing workloads before enabling any feed. Do not assume namespace isolation
provides separate node resources. Test the actual image under these limits;
review/restrict source caps or increase node capacity if the accepted 512 MiB
maximum feed cannot stay within this envelope. Increasing concurrency multiplies
working memory and scratch and requires a new measurement.

R2 retention grows with every distinct raw-byte revision plus its detail and
summary assets, even when a timestamp-only change does not request a rebuild.
Retain published evidence; this benchmark adds no cleanup policy for R2 or durable
snapshots.

## Automated evidence mapped to acceptance

| Requirement | Automated evidence | Remaining production evidence |
| --- | --- | --- |
| Exact raw bytes, BOM/whitespace and conditional GET | `conditional_get_archives_exact_bom_whitespace_and_checksum`, `unchanged_and_last_modified_checks_create_no_file` | Production source validators and raw-object readback |
| Missing validators, stalled body, 429/5xx, truncation, redirects, size caps | `missing_validators_length_and_stalled_body_are_handled`, `truncation_oversize_redirects_rate_limits_and_outages_fail_closed`, retry-after bounds tests | Production network timing and backoff monitoring |
| Schema/date/count/ID completeness; nullable evidence and corrections | Source parser failure tests and golden fingerprints; explicit null-versus-missing wire tests | Reviewed existing election identity and expected real feed bounds |
| Fenced claims, cancellation, bounded shutdown and disabled behavior | `rpc_worker` TCP boundary/lease/shutdown tests | Real service grants, competing workers and rollout drain drill |
| Original archive resume; failure before DB activation | Publication pipeline scenarios and `offline_disk_pipeline_publishes_complete_snapshot_and_fenced_pointer` | Interrupted import recovery against real Postgres/R2 |
| Immutable checksum collisions, upload failures and safe pointer races | Storage collision/failure tests; create/replace CAS races; post-PUT acknowledgement recovery | Atomic conditional writes on deployed R2 endpoint |
| Reader-compatible assets, decimal revisions, fingerprints | Exact golden asset bytes, numeric/Unicode fixtures and large revision tests | Existing open ballot page changes without refresh; restricted-view gates |
| Semantic change versus source-time-only rebuild behavior | Fingerprint variants demonstrate semantic equality/difference | Existing coordinator SQL integration/dirty revision readback |
| Bounded resources | Local real/enlarged reports; offline disk-backed integration test in normal Rust suite | Actual image/node memory, scratch, CPU and latency acceptance |
| Secret isolation and supply-chain checks | Trivy repository/image scans, RustSec audit, locked builds, SHA-pinned actions, Clippy/fmt/docs/tests | Namespace secrets, rotation and actual RBAC/network exposure review |

The Rust suite uses fakes for coordinator state and object storage. It must not
be described as testing all production SQL transaction guards, authentication,
visibility changes, or Cloudflare CDN behavior. Keep the reference collector and
website contract tests as compatibility oracles.

## Activation gate

Before enabling production, record: approved schema/code on website main;
reviewed election UUID/date/source/window; one active implementation after
reference-worker pause/drain; working service-only credentials and narrowly
scoped bucket writes; controlled import; exact raw/asset/DB/pointer readback;
production CORS/content-type/cache freshness no worse than 30 seconds; real R2
conditional-write checks; public and restricted browser behavior; monitored
backlog/publication lag and exercised alert delivery; replay/quarantine/pointer
repair/rollback drills; named operator and acceptance record. An unchanged
source report is normal and is not an alert condition. Namespace creation,
image publication and a disabled Deployment do not satisfy this gate.
