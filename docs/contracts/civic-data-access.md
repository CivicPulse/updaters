# Authenticated civic data access

Both services use the same `civpulse-civic` library, operation schemas and handlers.
The [coverage matrix](civic-resource-coverage.md) accounts for every live public
table and the derived county-statistic view. A reviewed allowlist contains 76
civic resources; five account/worker/upload-state tables use existing guarded
operations or remain internal. New tables are never exposed automatically.

Every civic API request, including discovery, is authenticated with a current
Supabase user. Anonymous users and anonymous Auth sessions are rejected. Every
backend request uses that caller's bearer and the configured publishable key.
No service-role credential is used. Existing database RLS, visibility-aware RPCs,
owner checks, administrator checks, constraints, audit triggers and storage
policies remain authoritative. No production schema or permission changes are
needed. OAuth discovery and private health probes remain protocol infrastructure,
not civic data endpoints; health is not routed publicly.

The public REST documentation endpoints, `GET /api/v1/openapi.json` and
`GET /api/v1/docs` (Swagger UI), describe the API without accessing civic data
or contacting Auth. All data and discovery operations below still require a
verified user token; Swagger UI supports bearer authorization.

## Discover and invoke operations

- MCP: `list_civic_resources` describes fields, complete primary keys, possible
  table operations and guarded workflows for each resource.
- REST: `GET /api/v1/civic-resources` returns the identical inventory.
- REST: `GET /api/v1/operations` returns the exact MCP operation catalog, including
  input schemas. `GET /api/v1/operations/{name}` describes one operation.
- REST: `POST /api/v1/operations/{name}` invokes any advertised operation with
  its JSON arguments. This includes all existing civic, relationship, geometry,
  legislative, document, meeting, evidence, finance, officeholder and image tools,
  plus the native extensions. Unknown operations cannot invoke arbitrary RPCs.

For example, call `save_person` with the same body used by MCP:

```json
{"request":{"operation":"create","slug":"example","full_name":"Example"}}
```

The caller must have existing administrator permission for that operation.
A capability listing describes what a resource supports, not what this user is
authorized to do. Database denials are never converted into success.

## Civic CRUD and search

| Operation | MCP | REST |
|---|---|---|
| Search/browse | `search_civic_data` | `GET /api/v1/data/{resource}?query=...&limit=20&offset=0` |
| Read one key | `get_civic_data` | `GET /api/v1/data/{resource}/{id}` |
| Create | `create_civic_data` | `POST /api/v1/data/{resource}` with a field object |
| Patch one row | `update_civic_data` | `PATCH /api/v1/data/{resource}` with `{"keys":{...},"data":{...}}` |
| Delete one row | `delete_civic_data` | `DELETE /api/v1/data/{resource}` with `{"keys":{...}}` |

MCP CRUD arguments include `resource`, using the table names in the inventory.
Reads, updates and deletes require every primary-key component and no extra keys.
For composite reads, use `POST /api/v1/operations/get_civic_data`, for example:

```json
{"resource":"contest_candidates","keys":{"contest_id":"<UUID>","person_id":"<UUID>"}}
```

Updates preserve omitted fields, never upsert and cannot change primary keys.
Creates never upsert. Generated fields and system timestamps cannot be written.
Required create fields and scalar types are checked before forwarding; database
constraints retain final validation, including foreign keys and numeric precision.
Bulk updates/deletes and arbitrary tables/columns/operators are unavailable.
A filtered-out update/delete returns 404, never a successful deletion/update.

Core record reads and discovery retain the existing visibility/listing gates in
`get_civic_record` and `search_site`. Core discovery uses existing full-text
semantics; use `search_site` for its advanced filters. Other table searches use
case-insensitive substring matching on text columns and exact equality filters.
Pass `filters` as a JSON object in MCP, or as a JSON-encoded query parameter in
REST. Null filters use `IS NULL`. Pages are ordered by the complete primary key;
limit is 1–100 and offset is 0–100,000. Private direct-table rows remain filtered
by RLS; admin-only raw reads also require an explicit admin preflight to avoid expensive rejected scans; some public projections are available only through their guarded readers.
Existing `/api/v1/organizations`, `/people`, `/elections`, `/me` and official
results routes remain compatible.

## Ownership, evidence and specialized resources

Records-request operations now include search, detail, form save, owner transfer,
adding/removing updates, links, fees and files, and authorized original-file access.
The existing database permits only admins to create requests; owners/admins can
edit and transfer existing requests. `save_records_request` retains the existing
form-replacement semantics: supply all desired editable values. It does not imply
partial-update behavior. Private requests and ownership/transfer identities retain
the existing manager-only projection. Generated stage history cannot be removed.
`manage_records_request_file` supports uploads up to 3 MiB inline, authorized
five-minute download URLs and owner/admin deletion. Larger original uploads use
the existing authenticated Storage API before attachment through the guarded RPC.
Failed metadata saves preserve private originals for reconciliation.

Stored SOS snapshot reads and reviewed ballot mappings now have their guarded readers and admin import/mapping operations. Source snapshots retain staging limits, publication compare-and-swap, and the prohibition on manual publication for collector-managed feeds. Automation configuration and leases are not exposed.

Voter-roll history and import workflows are admin-only. Completed snapshots stay
immutable. Begin/append/complete preserve original checksums, source columns,
expected counts, correction lineage and the existing 1,000-row/2 MiB batch limit.
Short-link form saves stay admin-only; deactivate with `active=false`. Audit
history uses the existing redacted reader and cannot be edited.

Evidence reports, observation ledgers, publication state and finance releases do
not acquire new generic writes or deletes. Their advertised import, correction,
publication, withdrawal or other guarded workflows remain the supported lifecycle.
Calls forbidden by existing permissions fail, including for administrators where
the database deliberately makes evidence immutable. Storage buckets and derived
views do not acquire arbitrary SQL-style CRUD.

All API responses carry `Cache-Control: no-store`. API errors are sanitized:
401 requires authentication, 403 is forbidden, 404 is unavailable, 400/422 is an
invalid request, 409 is a conflict, and 502/503 is an unavailable service. MCP
retains tool error envelopes. API JSON bodies have the same 40 MiB transport cap
as MCP; individual operations retain their smaller domain limits.

## Validation

Workspace tests cover the complete frozen V2 oracle, every resource disposition,
exact catalog parity, route/method authentication, anonymous-session rejection,
RLS-denied and filtered-out writes, composite identities, owner workflow forwarding
and immutable evidence. The isolated-database contract in
`crates/civpulse-civic/tests/permissions.sql` verifies real website RLS for admin,
owner and unrelated users. Run it only in a disposable `civic_api_permissions`
database populated from the website schema and audit configuration. Synthetic
users/content are enclosed in a transaction and rolled back.
