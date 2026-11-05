# Production API edge

For the Thor dev/test and DigitalOcean production split, see the
[hosting overview](../README.md).

Traffic: HTTPS webapi.civpulse.org → Cloudflare Tunnel → Traefik ClusterIP
in civpulse-edge → civpulse-webapi ClusterIP in civpulse-prod.

There is no LoadBalancer, NodePort, host port, or public cluster ingress.
TLS terminates at Cloudflare; the connector establishes an outbound encrypted
tunnel. HTTP is used within the cluster. NetworkPolicies require a supporting CNI.
Keep worker firewalls closed to public inbound application traffic.

## Provision and deploy

These files do not create Cloudflare account resources or publish an image.
This uses a dedicated remotely managed tunnel, keeping Thor's connector and
credentials separate.

1. Build/push the API image with the root Dockerfile:
   `docker build --build-arg TOOL=civpulse-webapi -t REGISTRY/civpulse-webapi:VERSION .`
   Replace the image in deploy/k8s/webapi.yaml with its immutable registry digest.
   Pin the cloudflared image to an approved digest too.
2. Create namespace civpulse-edge. Install the pinned chart:
   `helm upgrade --install traefik traefik --repo https://traefik.github.io/charts --version 42.3.0 --namespace civpulse-edge --create-namespace -f deploy/edge/traefik-values.yaml`
3. In Cloudflare One, create a remotely managed tunnel for this cluster.
   Create Secret cloudflare-tunnel in civpulse-edge with key token using your
   secret manager. Do not commit the token or put it in command history.
4. Configure the tunnel's published application:
   hostname webapi.civpulse.org, service
   http://traefik.civpulse-edge.svc.cluster.local:80.
   Preserve the original Host header (or explicitly use webapi.civpulse.org).
   Create the proxied DNS record to the tunnel as part of this configuration.
   Use a catch-all http_status:404 for unmatched tunnel ingress.
   Enable Always Use HTTPS at Cloudflare.
5. Use namespace civpulse-prod and Secret webapi-supabase with keys url and
   publishable-key for the same Supabase project used by the website.
   Use a new sb_publishable_ key; legacy anon JWT keys and service-role/secret keys
   are deliberately rejected by the application.
6. Apply deploy/k8s/webapi.yaml, deploy/edge/cloudflared.yaml and
   deploy/edge/network-policy.yaml. Check both deployments' rollout status.
7. Verify unauthenticated GET /api/v1/me and /api/v1/organizations return 401.
   Verify a real user's token returns their own ID. Compare organization results
   with the Data API under the same token; test both admin and ordinary users.
   Do not enable caching for API responses. Responses include Cache-Control:
   no-store. TCP probes check the process only, not Supabase availability.

## Future services

Add another ClusterIP service and an Ingress with host webapi.civpulse.org,
class civpulse-internal and a distinct Prefix path, e.g. /geo/v1. The current
API owns /api/v1 and retains that prefix. No path stripping is configured.
Unmatched paths are Traefik 404s. Every future service must authenticate its
requests and enforce its own resource permissions; this tunnel is connectivity,
not user authentication.

Never route a dashboard or unauthenticated health endpoint publicly. The API
has no HTTP auth exceptions. Traefik's internal health probe is not exposed by
its ClusterIP service.

## References

- [Cloudflare Kubernetes tunnel deployment](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/deployment-guides/kubernetes/)
- [Traefik chart](https://github.com/traefik/traefik-helm-chart)
- [Supabase grants and RLS](https://supabase.com/docs/guides/api/securing-your-api)

## Deployed 2026-11-04

- Cluster: k8s-1-37-0-do-2-atl1-1793724241236
  (6a0f9045-853a-451b-b013-9a7202e76854).
- API namespace: civpulse-prod; edge namespace: civpulse-edge.
- API image: registry.digitalocean.com/hatchk8s/civpulse-webapi@sha256:dc8b99be4e6e53dec70d6131b41434c24c3b0c112f21043b3c24dfdf5b61d3f1.
- Tunnel: civpulse-k8s-prod, b0e4ea46-c08a-4dfc-a6e2-1f506d4cf0ba.
- Proxied CNAME webapi.civpulse.org points to that tunnel.
- Cloudflare account: 6d837580a4d0641139ecada9e74076b8;
  zone: f94e4b3b22ccced560feebb9c7829a86.
- Supabase project is the existing production website project; publishable key
  and tunnel token are held only in Kubernetes Secrets.
- Pull credentials: existing registry-hatchk8s Secret in civpulse-prod.
- Initially added temporary node pool civpulse-api (05167687-0ad8-4422-835b-17d585ccc3dc),
  one g5-2vcpu-4gb-50gb worker, approximately $58.79/month.
  The original pool had insufficient memory reservations available; its
  autoscaler could not expand because DigitalOcean lacked regional capacity.

The committed image digests and manifests reproduce this release. For updates,
publish a new immutable image, change its digest, apply the manifest and check
rollout status. Roll back using the previous digest or
`kubectl -n civpulse-prod rollout undo deployment/civpulse-webapi`.

Production verification completed:
- API, Traefik and cloudflared each report 2/2 available replicas.
- HTTP redirects to HTTPS.
- Missing/invalid bearer credentials and unknown API routes return 401.
- Responses carry Cache-Control: no-store.
- A real temporary ordinary user's token returns the correct user ID.
- The API organization listing matches the direct Data API under the same token
  (100 rows); that user cannot see the admin allowlist.
- Temporary test accounts were deleted. CAPTCHA configuration was unchanged.
- Local formatting, Clippy, workspace tests and container startup check passed.

## Consolidated worker capacity

On 2026-11-04, after dev/test moved to Thor, production was consolidated onto
the three existing s5-2vcpu-2gb-50gb shared workers in pool-ld8qj5nlf
(85c2d144-8a70-445c-b560-836a77d50f4e). The temporary dedicated civpulse-api
pool was drained and removed. Shared 4GB/6GB replacement pools could not be
provisioned because DigitalOcean reported insufficient regional capacity.

Baseline worker cost is $103.95/month instead of $162.74/month, saving
$58.79/month (36%). Autoscaling keeps three workers and allows up to four;
the four-worker ceiling is $138.60/month. These figures exclude storage,
control plane, traffic and taxes. Retired dev/test volumes remain for rollback.

API, cloudflared and Traefik replica pairs use required hostname anti-affinity
and minAvailable=1 disruption budgets. Rolling API/tunnel updates keep the old
ready replicas until replacements are ready. Traefik now reserves 50m CPU and
128Mi RAM per replica. Every pair runs on separate workers. Current production
reservations, including per-node agents, fit on two workers if one fails;
this is a reservation calculation rather than a peak-load guarantee.

The machine-readable pool configuration and verification receipt are recorded in
[capacity.json](../k8s/capacity.json). Regional supply limits can still prevent
autoscaler expansion; monitor scheduling failures and memory pressure.

## MCP service

The V2 MCP Deployment shares this tunnel and Traefik at
`https://webapi.civpulse.org/mcp/v2`. Its separate Ingress owns only its MCP and
OAuth metadata paths; `/api/v1` stays with the API. See
[the MCP runbook](../../tools/mcp/README.md) for the pinned release, authentication
checks and independent rollback. No new DNS, tunnel or public listener is needed.
