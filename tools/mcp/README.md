# Civic Pulse V2 MCP

Standalone Rust HTTP replacement for the compact Civic Pulse Supabase Edge
Function. It serves the 79 upstream V2 tools plus native civic CRUD/search and workflow extensions, including fee reports, obligations, officeholder observations and checked
meeting-source imports, using the same PostgREST RPCs, tables,
Storage buckets, document archive and Georgia legislative source adapter.
It does not forward calls to the old MCP server or require Bun/Deno at runtime.

The port targets `kerryhatcher/www.civpulse.org` commit
`870a12dc60c780a4b0312060988b187d230b339f`, `apps/mcp/src` and
`apps/web/src/lib/legislation.ts`. The frozen tool schemas, descriptions,
annotations, trimming rules, legislative fields and turnout evidence schema
are under `crates/civpulse-civic/reference/`. Existing database migrations, visibility RPCs, RLS,
admin checks, audit trails and storage policies remain required. This crate
creates no schema and uses no service-role key.

## Run

Set the required variables in `.env.example` in the process environment, then:

```sh
cargo run --locked -p civpulse-mcp -- --check
cargo run --locked -p civpulse-mcp
```

The canonical `MCP_RESOURCE_URL` includes `/v2`. Its path and the equivalent
path with `/functions/v1` removed are accepted, including a trailing slash.
The legacy 128-tool catalog is not exposed. Protected-resource metadata is
available at `<resource>/.well-known/oauth-protected-resource` and the root
well-known aliases. `/healthz` is a process probe, not a backend readiness check.

The transport is stateless Streamable HTTP with JSON responses. Clients POST
JSON-RPC and accept both `application/json` and `text/event-stream`. Initialize,
ping, tool discovery and calls are supported; notifications return 202. GET/DELETE
return 405 because there is no persistent SSE stream or session to terminate.
Request bodies are bounded to 40 MiB. The public host and browser origin checks
match the existing deployment. Route the canonical host through your ingress;
health probes may use `Host: localhost`. Ingress must preserve the canonical
request path and Host, and allow the body size and up to 60-second backend calls.

Each request validates the user's token through Supabase Auth and checks issuer,
audience, role, subject and expiry; asymmetric tokens also verify against JWKS.
The same caller bearer token and public key reach PostgREST and Storage, preserving
RLS. Redirects are disabled. Optional archive/Georgia calls have separate bounded
timeouts; Civic Pulse credentials never reach the Georgia service.

## Uploads and writes

Create/update tools keep omitted fields and explicit nulls distinct; updates
never upsert. Relationship selectors, legislative tables and RPC names are
allowlisted. Multi-batch legislative imports retain partial results and offsets.
Inline meeting uploads preserve stored originals on uncertain metadata writes.
Resumable uploads keep the existing database-backed lease, creation intent,
chunk checksums, HEAD reconciliation and receipt rules. A lost creation Location
returns `creation_uncertain` without another POST. Finalization checks original
size, MIME signature and SHA-256 before publishing attachment metadata. Cancelling
a session retains the existing partial-TUS cleanup limitations.

The document archive remains optional. Publishing creates a public copy, just as
before, and requires the archive's independent operator configuration. No data,
credentials, live tokens or private documents are bundled in the fixtures.

## Verification and cutover

```sh
cargo test --locked -p civpulse-mcp
cargo clippy --locked --workspace --all-targets -- -D warnings
docker build --build-arg TOOL=civpulse-mcp -t civpulse-mcp:dev .
docker run --rm --read-only civpulse-mcp:dev --check
```

Compatibility tests replay 142 mocked HTTP request scenarios captured by invoking the actual
TypeScript tools over an in-memory MCP transport. They compare backend method,
path, query, arguments and returned JSON, and assert caller credential forwarding.
Refresh the reference files only after reviewing changes in the source checkout:

```sh
bun tools/mcp/scripts/export-reference.ts /path/to/www.civpulse.org
```

Bun and the source repo's installed dependencies are needed only for this export.
Parser fixtures compare the versioned Simbli adapter against the TypeScript
implementation. The tests also exercise HTTP authentication and protocol gates, admin rejection,
upload uncertainty/reconciliation and schema rejection. Mocked tests do not prove
production database policy or external-service availability.

`deploy/k8s/mcp.yaml` supplies a Deployment and Service, rather than a batch Job.
Create its `civpulse-mcp` Secret from the environment contract and configure your
image/ingress. Before switching traffic, exercise the deployed candidate with a
normal user and an admin against the existing Supabase schema, including a full
resumable upload and document publication in a nonproduction environment. Keep
the current Edge Function available for rollback; route V2 to the new service
only after those checks. No production configuration or traffic is changed by
this repository change.

The reference catalog and port derive from Kerry Hatcher’s MIT-licensed Civic
Pulse source; see the repository LICENSE. Authentication tests generate disposable signing keys in memory for each test.

## DigitalOcean deployment

Production shares the API's cluster context
`do-atl1-k8s-1-37-0-do-2-atl1-1793724241236` and namespace `civpulse-prod`.
The canonical MCP resource is `https://webapi.civpulse.org/mcp/v2`.

`deploy/k8s/mcp-production.yaml` defines two replicas on different workers,
a ClusterIP Service, a disruption budget, and ingress limited to the existing
Traefik pods in `civpulse-edge`. It references the existing `webapi-supabase`
Secret (`url` and `publishable-key`) and `registry-hatchk8s` pull credentials.
The optional archive and Georgia reader use the existing production services.

`deploy/k8s/mcp-ingress.yaml` adds a separate standard Kubernetes Ingress using
class `civpulse-internal` and Traefik's `web` entrypoint. It preserves paths:
`/mcp/v2`, `/functions/v1/mcp/v2`, and `/.well-known/oauth-protected-resource`.
The canonical and RFC path-scoped OAuth discovery URLs are supported. `/healthz`
is available to internal pod probes only; it has no public route. The existing
`/api/v1` route and Cloudflare Tunnel configuration are independent.

Publish a checked image, pin its registry digest in the production manifest,
then apply the workload separately from the public route. Wait for both replicas,
verify the candidate via port-forward, and only then apply the Ingress. Check the
public metadata, authentication challenge, authenticated catalog/whoami/read,
ordinary-user admin rejection, and the existing API route. All MCP responses
include `Cache-Control: no-store`.

For an initial-deployment rollback, delete only the `civpulse-mcp` Ingress to
remove public MCP routing, then remove its Deployment, Service, PDB and
`mcp-ingress` NetworkPolicy if required. Keep the API resources and Secrets.
For a later image rollback, restore the previous immutable image digest and
wait for rollout, or use `kubectl rollout undo deployment/civpulse-mcp` in the
explicit production context/namespace. The original Supabase Edge Function
remains at its existing hostname throughout.

### Deployed release

The production rollout and public smoke checks passed on 2026-11-05 UTC
(2026-11-04 America/New_York). See
[the immutable release receipt](../../deploy/k8s/mcp-release.json).
Both replicas became ready on separate existing workers; no new worker pool,
load balancer, tunnel, hostname or registry subscription was provisioned.

The registry's plan permits five repositories and all are occupied. MCP therefore
uses a distinct `mcp-sha-...` tag in `hatchk8s/civpulse-webapi`, pinned by digest
in the Deployment; the API's existing tags and digest are retained.

The public checks verified OAuth metadata and issuer, authentication challenges,
the exact 79-tool catalog, normal-user identity, a live visibility-aware civic
search, admin-only account-read rejection, public health-path exclusion and
browser preflight. The same temporary normal user token also succeeded on the
existing API. Both temporary test accounts were removed. No production content
writes, document publication or upload sessions were created during validation.
Admin writes and upload recovery retain their local compatibility/recovery test
evidence; these workflows were not exercised against production content.

To rerun the read-only public checks:

```sh
python3 tools/mcp/scripts/smoke-production.py
```

Set `MCP_SMOKE_TOKEN` to a normal user's access token in the environment to also
check authenticated MCP/API reads; optionally set `MCP_SMOKE_USER_ID` to assert
the exact account. Never put bearer tokens in command arguments or commit them.
The script does not create accounts or content. For an internal candidate,
port-forward the MCP Service and add `--base http://127.0.0.1:18789 --skip-api`.

PR validation upgraded the initial runtime to the workspace's digest-pinned
nonroot distroless Debian 13 image. Its Trivy HIGH/CRITICAL vulnerability and
secret scan passed before publication and rollout. The replacement completed a
zero-unavailable rollout, and the authenticated public MCP/API smoke checks
passed again. The receipt records the new image/source and prior digest.


## Complete authenticated civic access

The shared `civpulse-civic` library now powers both MCP and the REST API. See
[the resource and operation contract](../../docs/contracts/civic-data-access.md)
for the reviewed schema inventory, CRUD routes, composite keys, search and
owner/admin permission rules. `list_civic_resources` describes available fields,
table operations and guarded workflows. API callers can discover the identical
catalog at authenticated `GET /api/v1/operations` and invoke any operation using
`POST /api/v1/operations/{name}` with its advertised JSON arguments.
