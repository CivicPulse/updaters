# Bounded source library API

`civpulse-results-source` implements issue #5 (parent #1). No function writes to
Supabase or R2. Production collection remains disabled pending operational
acceptance. The pinned upstream oracle remains the compatibility authority.

## Integration

```rust,ignore
let result = civpulse_results_source::download(
    &job.source_url, &reviewed_exact_url, &raw_path,
    &Validators { etag: job.etag, last_modified: job.last_modified },
    &Limits::default(), &cancelled,
).await?;
// Download::Unchanged => existing unchanged RPC; no source spool created.
// Download::Downloaded(receipt) => validate, then archive exact raw bytes.
let prepared = civpulse_results_source::prepare(
    &raw_path, &new_scope_directory,
    &Expectations {
        election_date: job.election_date,
        expected_jurisdictions: Some(160), // reviewed per configured feed
        expected_items: Some(4_178),       // reviewed for the chosen receipt
    },
    &limits, &cancelled,
)?;
```

`prepare` is synchronous and cooperatively checks an `AtomicBool` cancellation
flag and total deadline. Run it on a blocking thread; set its flag and join the
thread during shutdown or lease loss. Do not drop a `spawn_blocking` handle and
assume it has stopped. The input and successful scope files belong to the caller.
The output directory must not exist, and failure removes it. Scope files are
fixed local `scope-N.json` names, never source-derived paths.

`Prepared` exposes `header: Map<String,Value>`, ordered `jurisdictions: Vec<Value>`,
`scopes: Vec<Scope {index,path,metadata}>`, `fingerprint: String`, and
`item_count: usize`. The state index is always zero, followed by original county
order, even when the root `localResults` field precedes `results`. Each file is
validated source JSON retaining unknown source fields and missing/null/zero
counts. Stage source items using their original scope/item indexes; do not sort
them as a side effect of fingerprinting.

`Receipt` contains exact-byte `sha256`, `bytes`, `retrieved_at`, `etag` and
`last_modified`. The caller adds reviewed `source_url`, `raw_key` and `raw_url`
when constructing the durable begin receipt. Resume must use the original job
receipt/archive and verify its bytes/hash, not fetch current upstream data.

`download` accepts only the exact supplied reviewed Georgia SOS HTTPS export URL;
redirects and nonidentity content encoding are refused. It uses conditional GET
and supports missing validators and length headers. One total deadline covers
network and spool writes; partial files are removed after failure/cancellation.
It returns `Error::Status {status,retry_after}` for HTTP failures. Durable job
retry/backoff with jitter belongs to the coordinator/worker; do not loop in this
function and hold an unrenewed lease. `retry_after` clamps numeric/HTTP-date
values to 1–3,600 seconds, defaulting to 60 for malformed/missing values.

## Bounds and validation

Default limits match the reviewed reference: raw <=512 MiB, one scope <=64 MiB,
header <=1 MiB, jurisdiction metadata <=128 KiB, <=160 jurisdictions, staging
<=50 items /2 MiB, download deadline 180s and parsing deadline 180s. Normalized
scope spools are individually capped at 64 MiB and cumulatively capped at 512 MiB;
raw plus normalized spool therefore requires at most 1 GiB, excluding filesystem
metadata and later public asset generation. The collector must configure its
whole-pipeline disk envelope separately. Batches are checked before any staging.

The parser reads buffered source bytes incrementally, deserializes one capped
scope, streams its canonical hash, writes a bounded scope file, then releases the
scope. It retains bounded header, metadata, and scope hashes, not a statewide
JSON value or every serialized batch. Duplicate JSON keys, duplicate source IDs,
invalid counts, incomplete JSON, changed root schema, wrong election date,
invalid/future source time, nesting >40, and completeness mismatches fail closed.
Explicit `expected_jurisdictions` and `expected_items` are optional for diagnostic
validation; production should supply reviewed expectations for its source.
Jurisdiction/contest/choice IDs must be nonempty and correctly scoped. Official
precinct evidence can contain an empty string ID (observed in the reviewed real
November feed); preserve it rather than inventing an identity. Nested identity
arrays still require string identities and reject duplicates within their parent.

`canonical(&Value)` returns canonical fingerprint bytes using UTF-16 object key
ordering, default ICU identity-array collation and ECMAScript `ryu-js` binary64
number serialization. `stringify(&Value)` instead preserves JavaScript property
enumeration/insertion order, also with ECMAScript numbers. Use the latter for
byte-compatible public asset JSON. It includes JSON array-index property ordering.
Serde `preserve_order` and `float_roundtrip` features are intentional. Do not
replace these functions with ordinary `serde_json::to_vec` when byte/fingerprint
compatibility matters. Golden tests include accented/mixed-case/non-BMP IDs,
supplementary keys, exponent thresholds, negative zero, escapes and nulls.

The exact-pinned ICU data and explicit English collation match the pinned Bun
reference on the Unicode fixture and entire reviewed real source. Upgrading ICU
or the reference runtime requires rerunning the golden and real-feed comparison;
locale-sensitive ordering is part of the persisted fingerprint contract. The
manifest pins collator, locale, normalization, properties, compiled data and ICU
provider/collection dependencies to their reviewed 2.1.x versions. These direct
constraints are intentional even where a dependency is only used transitively:
`cargo update` and an MSRV change must not silently replace collation semantics.

## Verification

Run formatting, Clippy with warnings denied, and locked workspace tests. The
source tests exercise actual Rust parser/fingerprints against upstream-generated
fixtures, exact raw checksums, successful 304, missing validators/lengths, 429/503,
truncation, redirects, oversized/chunked responses, stalled bodies, deadlines,
invalid schemas/counts/IDs, nesting, completeness and cleanup.

Local resource validation uses no credentials or external writes:

```sh
cargo run --release --locked -p civpulse-results-source --example validate -- /path/to/source.json
/usr/bin/time -v target/release/examples/validate /path/to/source.json
```

The example intentionally expects the reviewed November 2026 identity and
160 jurisdictions/ 4,178 items. It is a diagnostic and resource tool, not an
activation command or production host measurement. Full pipeline measurements,
production R2 conditional writes/CORS/cache verification, monitoring/recovery and
browser readback remain later acceptance gates.

Local Linux x86_64 (AMD Ryzen 7 5800X) release validation measurements (2026-11-03, Rust 1.99.1;
these exclude download, RPC, R2 upload and public asset generation):

| Input | Exact bytes | Scopes / items | Wall time | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Reviewed official November export | 46,363,141 | 160 / 4,178 | 3.67s | 78,404 KiB |
| Same feed with synthetic 32 KiB padding per item | 174,387,013 | 160 / 4,178 | 9.26s | 81,660 KiB |

Official raw SHA-256:
`f464d3b037a82d594c893aef072042530a84b6b1cb3edb76b1e3873bed2dc3ba`.
Its Rust semantic fingerprint is
`08145568d3e4ca710d32f831d71688205bd63ed1365dd2bdaff619e1e9c99bbb`,
independently matched against the pinned Bun `prepare` implementation on the
same bytes. Synthetic padded input SHA-256:
`4f4ce3c4856a261096848f7b1f033e69ab4e1501f61099411ea771a12a89d2c9`;
its Rust fingerprint is
`57b28cf5432925af8ba78a93be57c31d7bd56b3c937b148f0ed59eef2e27fee2`.
These are local validation measurements, not a chosen Kubernetes host/resource
or cost acceptance. The synthetic input is not additional election evidence.

The credential-free production CLI wrapper is available with:

```sh
cargo run --locked -p civpulse-results-collector -- validate /path/to/source.json 2026-11-03 160 4178
```

The final two completeness arguments are optional and must be supplied together.
Successful validation prints only JSON counts, fingerprint and elapsed time;
validation errors omit local paths and source contents. SIGTERM/Ctrl-C signals
cooperative cancellation and waits for the blocking parser and its temporary
spool to exit. Nonregular inputs are rejected so a FIFO/device cannot leave the
blocking parser waiting indefinitely. Validation reads no credentials and never
activates collection, even if production configuration variables are present.
