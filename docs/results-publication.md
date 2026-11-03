# Durable results publication

The Rust `Publisher` consumes the existing fenced job/RPC state machine described
in [the pinned contract](contracts/results-collector.md). Production collection
remains disabled until acceptance is recorded. Source polling remains owned by
Supabase; this worker only consumes durable jobs.

## Enabled-worker configuration

Set `SOS_COLLECTOR_ENABLED=true` only for a reviewed controlled import or accepted
feed. Disabled daemon startup needs no secrets. Enabled collection requires:

| Variable | Meaning |
| --- | --- |
| `CIVPULSE_ENV` | Exactly `dev`, `test`, or `prod` |
| `SUPABASE_URL`, `SUPABASE_SERVICE_ROLE_KEY` | Service-only coordinator RPC access |
| `SOS_ELECTION_ID` | Reviewed existing election UUID; other queued elections are rejected |
| `SOS_ELECTION_DATE` | Exact reviewed `YYYY-MM-DD` |
| `SOS_SOURCE_URL` | Exact reviewed official SOS export URL |
| `SOS_EXPECTED_JURISDICTIONS`, `SOS_EXPECTED_ITEMS` | Positive reviewed completeness counts; jurisdiction cap 160 |
| `SOS_SPOOL_ROOT` | Writable disk-backed scratch root, default `/tmp` |
| `R2_ENDPOINT` | Credential-free account HTTPS S3 endpoint |
| `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` | Environment-scoped object read/write credentials |
| `R2_BUCKET`, `R2_PUBLIC_ORIGIN` | Must match the fixed environment pair below |

| Environment | Bucket | Public origin |
| --- | --- | --- |
| dev | `civpulse-files-dev` | `https://files-dev.civpulse.org` |
| test | `civpulse-files-test` | `https://files-test.civpulse.org` |
| prod | `civpulse-files` | `https://files.civpulse.org` |

The constructor rejects cross-environment buckets/origins. Public domain setup,
CORS, and effective CDN caching are deployment acceptance gates; configuration
alone does not establish that these domains are reachable. Never copy production
credentials into dev/test namespaces. R2 credentials need object read/write only,
not bucket deletion or account administration.

The workspace minimum Rust version is 1.94.1 for the current maintained official
AWS SigV4 signing library. The reproducible build toolchain remains pinned to
1.99.1. S3 requests use region `auto`, header signatures, precomputed payload
SHA-256 and bounded streaming bodies; credentials never appear in query URLs.
No dependency error chain, response payload or credential URL is logged.

## Ordering and recovery

Downloads are validated before public upload. Exact raw bytes are archived under
their SHA-256 key; create-only collisions are verified by streaming hash and size
readback through S3. A resumed import downloads its original archived source,
validates its original receipt/key/hash/size, and never mixes newer upstream data
into existing staging. Blocking validation owns its scratch directory until it
stops, even after asynchronous cancellation. All scratch state is expendable;
Postgres and R2 own durable recovery.

Discovery manifests and pointers are bounded to 1 MiB, with reserved space for
publication fields checked before database activation.

The worker stages bounded batches through `stage_sos_results_job`, then uploads
and verifies every immutable detail, summary and manifest object. Only afterward
can `publish_sos_results_job` activate the complete database snapshot and outbox.
The worker confirms that outbox, reads its manifest, then publishes `latest.json`
using an atomic S3 `If-Match` or initial `If-None-Match: *`. Decimal revisions are
compared numerically without floating point. A delayed writer rereads on 412 and
cannot overwrite a newer revision. The pointer uses the original receipt and
revision-specific immutable URLs, so related browser reads cannot mix revisions.

After a database-commit crash, a pointer-phase job skips downloading, staging and
asset generation. After pointer PUT but before acknowledgement, a matching
revision/snapshot is acknowledged without rewriting or reimporting. Asset
failures leave the prior active database snapshot and discovery pointer intact.
Lease loss cancels the processor; every mutation uses its fenced lease identity.
No direct table writes or per-batch rebuild webhooks are used.

## Verification

`cargo test --locked -p civpulse-results-collector` includes exact generated asset
byte comparisons with the pinned Bun oracle, immutable checksum collisions,
create/replace pointer races, stale/equal revision handling, interrupted staging
from the original receipt, failed-asset activation prevention, database commit
crashes and pointer acknowledgement crashes. Tests use isolated local doubles.

For explicit **isolated test bucket** S3 acceptance, inject test credentials from
the secret store and run:

```sh
SOS_R2_TEST_ACCEPTANCE=true cargo run --locked -p civpulse-results-collector \
  --example r2_acceptance
```

This opt-in example writes synthetic evidence only to `civpulse-files-test` under
`elections/00000000-0000-4000-8000-000000000004/`. It verifies actual signed S3
create-only writes, exact collision checksum readback, discovery publication,
stale revision rejection and repeat acknowledgement. It deliberately retains
those synthetic objects for inspection and never configures a public domain or
changes production. Passing it does not establish CORS/CDN/browser acceptance.
