# Georgia results collector compatibility contract

Parent: [CivicPulse/updaters#1](https://github.com/CivicPulse/updaters/issues/1).
The compatibility oracle is website commit
[`0408add4c47a8549c9ad76c680d9a5779a58a050`](https://github.com/kerryhatcher/www.civpulse.org/tree/0408add4c47a8549c9ad76c680d9a5779a58a050/apps/results-collector).
`reference.json` records its source hashes. This document does not establish that
production migrations, credentials, collection, or public readback are accepted.
Collection must remain disabled until recorded acceptance completes.

## Ownership and identity

Supabase's authenticated `poll-election-results` function and `pg_cron` own the
five-minute source checks and durable enqueue operation. Rust polls that queue;
it must not create an independent source scheduler, schema, or rebuild webhook.
The existing election UUID, expected date, saved exact export URL, public
visibility, enabled flag, and explicit polling window require review. They are
not inferred from names. Do not modify unrelated election records.

The target Kubernetes context is `k8s-1-37-0-do-2-atl1-1793724241236`. Development,
test, and production use `civpulse-dev`, `civpulse-test`, and `civpulse-prod`.
Environment isolation must include separate credentials/configuration and test
fixtures; sharing the host does not authorize development writes to production.

## RPC state machine

All RPCs are POST `/rest/v1/rpc/<name>` using server-only service-role headers.
The reference deadline is 60 seconds. Every leased operation below receives
`p_job_id` and `p_lease_id`; stale workers must stop mutating once renewal fails.
Never bypass the RPC guards with direct table writes.

| Operation | Additional request fields | Result/use |
| --- | --- | --- |
| `claim_sos_results_job` | none | nullable job: `job_id`, `lease_id`, `election_id`, `election_date`, `source_url`, validators, `phase`, optional `snapshot_id`, `revision`, `manifest_key`, `receipt` |
| `renew_sos_results_job` | none | boolean; false means lease lost |
| `unchanged_sos_results_job` | `p_etag`, `p_last_modified` | completes a valid HTTP 304 check |
| `begin_sos_results_job` | `p_sha256`, `p_header`, `p_jurisdictions`, `p_receipt` | `snapshot_id`, `published`, original durable `receipt` |
| `stage_sos_results_job` | `p_snapshot_id`, `p_items` | idempotent bounded source items |
| `publish_sos_results_job` | `p_snapshot_id`, `p_fingerprint`, `p_manifest_key` | atomically activates complete snapshot and pointer outbox |
| `confirm_sos_results_pointer` | none | nullable current publication: `snapshot_id`, `manifest_key`, decimal `revision`, `published_at` |
| `complete_sos_results_pointer` | `p_revision` | acknowledges matching pointer |
| `fail_sos_results_job` | bounded sanitized `p_error`, optional `p_retry_after` | durable retry/quarantine |
| `get_sos_collection_health` | none | read-only service operational state |

Reference queue polling is 15 seconds, lease duration five minutes, and renewal
interval 30 seconds. Renew during network work, not just between stages. Once
staging starts, resume from the archived raw key, exact checksum/size and original
receipt, even if upstream has moved on. A pointer-phase job does not redownload or
restage. Shutdown stops claims, checkpoints safely, and leaves unfinished work
recoverable through durable state. Dropping an async future alone does not cancel
blocking parsing or undo a remote write.

The database rejects older source timestamps and equal-time conflicting reports.
An equal timestamp is accepted only for the current automation-owned fingerprint.
Preserve the manual-publication guard. Meaningful publication dirties the existing
frontend revision once, while root-source-time-only/formatting changes do not.

## Bounded source and fingerprint

Keep exact downloaded BOM, whitespace and bytes for the raw SHA-256/archive.
Conditional GET is authoritative even after a metadata HEAD check. Refuse
redirects, enforce the exact reviewed source URL, and handle missing validators
and HEAD-not-supported responses through the coordinator contract. HTTP 304 is a
successful unchanged check. Bound deadlines, retries, concurrency and disk.

Reference bounds: 512 MiB download; 64 MiB scope; 1 MiB header; 128 KiB scope
metadata; 160 jurisdictions; 50 items and 2 MiB per staging batch; 4 MiB summary.
Review changed source shape/count expectations explicitly rather than dropping
records. Numeric counts are null or nonnegative JavaScript-safe integers;
missing is distinct from null and zero. Never infer winners, people, certification,
turnout denominators, or sum overlapping state/county/precinct/method counts.

The fingerprint is SHA-256 of canonical `{header,scopes}`. Remove only root
`createdAt` from the header. Each scope entry is `{id,hash}` where hash is SHA-256
of its canonical full source scope. Sort scope entries using the reference's
`localeCompare`. Canonical objects sort keys by JavaScript UTF-16 ordering.
Arrays named `ballotItems`, `ballotOptions`, and `precinctResults` sort by string
`id`, and `groupResults` sorts by string `groupName`, only when every identity is
present and unique. Other arrays retain order. Identity sorting uses JavaScript
`localeCompare`, not byte or ASCII order. Number/string escaping must match
`JSON.stringify`; do not substitute arbitrary serde serialization for canonical
fingerprints. Root timestamp changes are ignored; labels, statuses, nulls, zeros,
unknown source fields and downward corrections remain meaningful.

## Public files and atomic pointer

Use existing R2 `civpulse-files` and public origin `https://files.civpulse.org`:

- `elections/<election>/raw/<exact-sha256>.json`
- `elections/<election>/revisions/<snapshot>/manifest.json`
- `elections/<election>/revisions/<snapshot>/state.json` and `state-detail.json`
- `elections/<election>/revisions/<snapshot>/counties/<source-id>.json` and `<source-id>-detail.json`
- `elections/<election>/latest.json`

Immutable writes are create-only; collisions require checksum and size readback
through uncached S3. Complete/verify all assets before database activation.
Manifest provenance includes source URL/time/hash/bytes, raw URL, retrieval time,
validators, fingerprint, and jurisdiction links. The discovery pointer adds
`revision` as a decimal string and `published_at`. Never convert revisions to
float or compare them lexicographically. Scope payloads preserve source indexes
and JSON. Summaries omit item `rankedChoiceResults` and option
`precinctResults`/`groupResults`; detail assets preserve them.

Only after commit may the worker publish the pointer. Read existing pointer/ETag
through S3, compare revision, reconfirm the DB lease/publication, and use atomic
`If-Match` or initial `If-None-Match: *`. On contention reread. An equal pointer
already written after a crash is acknowledged without reimporting. Database-only
checks or CDN-cached ETags do not fence expired writers. Verify actual deployed
R2 conditional operations, not just mocks.

Immutable cache policy is `public,max-age=31536000,immutable`; pointer policy is
`public,max-age=15,s-maxage=15,must-revalidate`. Verify effective custom-domain
CORS/cache freshness <=30 seconds. Public projections never contain curated
profile mappings or restricted metadata. Visibility controls admission; already
public files and in-flight uploads cannot be recalled. Retain evidence and
recoverable work; no blanket bucket lifecycle deletion.

## Golden fixtures and remaining gates

Run `bun scripts/contracts/generate.ts` to reproduce synthetic fixtures from the
pinned upstream implementation. It performs public source downloads only and
uses an in-memory object store; no credentials or production writes. Run
`cargo test --locked -p civpulse-results-contract` to verify receipts, wire
round trips, summary/detail shape, null/zero/missing preservation and arbitrary
precision decimal revisions. Fixtures are synthetic and are not election evidence.

The golden fingerprints were generated by upstream, not by Rust. Unit 3 must
compare its actual parser/fingerprint implementation against these outputs, and
unit 4 must compare generated assets. Current golden tests establish the oracle
and wire types, not completed Rust collection or production acceptance.
Additional acceptance needs real and oversized feeds; malformed/truncated/schema
and size-limit tests; lease/crash/race fault injection; real R2 checks; host
resource/cost measurement; alert/recovery drills; and an already-open ballot page
updating without refresh. Record the reviewed UUID, window, operator, and evidence
before activation. Production remains disabled until these gates pass.
