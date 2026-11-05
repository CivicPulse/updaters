# Service deployment and shared ingress

Each executable service crate is built into its own container image and deployed
as an independent Kubernetes workload. HTTP services have their own Deployment
and ClusterIP Service, so releases, scaling, resource limits, health checks, and
rollback can be managed separately. Shared library crates are linked into those
executables and do not receive workloads.

Within each environment, HTTP services share one router/ingress and one public
FQDN. The router directs distinct, owned path prefixes to the appropriate
ClusterIP Service. For example, a future `/api/service-a/` route can target
service A while `/api/service-b/` targets service B on the same hostname. These
are illustrative future paths. Production uses `webapi.civpulse.org`, with
`/api/v1` reserved for `civpulse-webapi` and `/mcp/v2` reserved for
`civpulse-mcp`, both preserved by Traefik. MCP also owns the matching
`/functions/v1/mcp/v2` compatibility alias and OAuth protected-resource metadata
paths. Its internal `/healthz` is excluded from public routing.

Maintain the hostname, TLS configuration, and route ownership together in the
environment's ingress configuration. A service release must not create a new
public load balancer, catch-all route, or overlapping prefix. Specify whether
its prefix is preserved or stripped, and test trailing slashes, redirects,
WebSocket support where needed, and authentication at the routed boundary.
Keep authorization within each service as well; sharing a hostname does not
grant one service access to another service's data or credentials. Restrict
backend ingress to the router and required internal callers with NetworkPolicy.
Keep metrics and administrative endpoints internal unless explicitly exposed.

Dev and test run in `civpulse-dev` and `civpulse-test` on Thor. Production
runs in `civpulse-prod` on DigitalOcean cluster
`k8s-1-37-0-do-2-atl1-1793724241236`. Its edge runs in `civpulse-edge`.
See the [hosting overview](../deploy/README.md) for contexts, storage and rollback.
Use separate environment hostnames and routes when public HTTP endpoints are
needed. Prefer environment-local routing to avoid cross-namespace backend
references; any shared ingress controller can serve those separate routes.
Production ingress uses an outbound Cloudflare Tunnel and internal Traefik
ClusterIP services, with no DigitalOcean load balancer. The
[edge runbook](../deploy/edge/README.md) describes the deployed routing.

## Election results collector

The results collector consumes durable Supabase jobs and publishes approved
public assets to R2. It has no inbound HTTP API and needs no public Service or
ingress route. Its read-only monitor is a separate scheduled workload using the
same selected image, with persistent observation and notification state. The
existing Supabase coordinator owns source scheduling; the shared-ingress design
must not introduce another source scheduler.

Future HTTP service crates should join the shared routing model without
requiring a public endpoint for the results worker. The existing
`files.civpulse.org` asset domain and website remain separate applications with
their existing contracts.
