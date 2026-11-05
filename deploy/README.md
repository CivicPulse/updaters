# Hosting and environment split

Last verified: 2026-11-04.

This overview covers the results collectors, their dev/test Supabase stacks,
and the REST API. Thor also hosts older Civic Pulse applications; their placement
is outside this deployment split.

## Deployment targets

| Environment | Host / Kubernetes context | Namespace | Running services | Database |
| --- | --- | --- | --- | --- |
| Development | Thor k3s, context `thor` | `civpulse-dev` | Results collector and isolated Supabase | Self-hosted PostgreSQL on Thor |
| Test | Thor k3s, context `thor` | `civpulse-test` | Results collector and isolated Supabase | Separate self-hosted PostgreSQL on Thor |
| Production | DigitalOcean Kubernetes, context below | `civpulse-prod` | REST API, results collector, collector monitor | Existing production website Supabase project, outside this cluster |
| Production edge | Same DigitalOcean cluster | `civpulse-edge` | Cloudflare Tunnel connector and Traefik | None |

The production context is:

```text
do-atl1-k8s-1-37-0-do-2-atl1-1793724241236
```

The DigitalOcean cluster is in `atl1`, with ID
`6a0f9045-853a-451b-b013-9a7202e76854`.

Dev and test each have Helm releases named `supabase` and `results`. Their
Supabase stacks include PostgreSQL, Auth, PostgREST, Storage, and Kong.
Production has the `results` release and the separately deployed REST API;
it does not use the dev/test self-hosted Supabase databases.

Namespace names alone do not identify an environment's active host. The old
`civpulse-dev` and `civpulse-test` namespaces still exist on DigitalOcean for
rollback, with their migrated workloads scaled to zero. Always pass an explicit
Kubernetes context when operating on either cluster.

## Routing and authentication

Production API traffic follows:

```text
https://webapi.civpulse.org/api/v1/...
  → Cloudflare
  → civpulse-k8s-prod tunnel
  → Traefik ClusterIP in civpulse-edge
  → civpulse-webapi ClusterIP in civpulse-prod
  → production Supabase
```

The connector makes outbound tunnel connections. Traefik routes host/path
combinations so future microservices can share the hostname under distinct path
prefixes. The current API owns `/api/v1`; the prefix is preserved.

The edge and API services are internal ClusterIP services. This deployment uses
no DigitalOcean load-balancer ingress, NodePort, or host port. TLS terminates at
Cloudflare, and HTTP is used between services inside the cluster.

Every API HTTP request requires a valid user bearer token. The API validates it
with Supabase Auth and uses the same token for database requests, so grants and
row-level security remain authoritative. The API uses a publishable key rather
than a service-role key. Cloudflare Tunnel provides connectivity; it does not
replace application authentication.

Dev/test Supabase gateways remain internal to Thor. No public dev/test REST API
endpoint is configured by these manifests. Their collector database URLs retain
the environment-specific service names:

```text
http://supabase-supabase-kong.civpulse-dev.svc.cluster.local:8000
http://supabase-supabase-kong.civpulse-test.svc.cluster.local:8000
```

See the [production edge runbook](edge/README.md) for tunnel configuration,
secrets, rollout checks, and adding routes.

## Data, credentials, and storage

Each environment has its own database credentials and R2 credentials. Keep
production credentials out of Thor's dev/test workloads. Collectors use their
environment's service credentials; the public REST API uses the caller's user
identity instead.

| Environment | R2 evidence bucket | Public file origin |
| --- | --- | --- |
| Development | `civpulse-files-dev` | `https://files-dev.civpulse.org` |
| Test | `civpulse-files-test` | `https://files-test.civpulse.org` |
| Production | `civpulse-files` | `https://files.civpulse.org` |

Thor's dev/test Supabase stacks use `local-path` persistent volumes: 10 GiB for
database data, 1 GiB for PostgreSQL encryption keys/configuration, and 1 GiB for
Storage files per environment. Preserve the key volume with the database.
The production monitor retains its DigitalOcean persistent volume.

Credentials belong in Kubernetes Secrets, not committed values or container
images. The production API reads `webapi-supabase`; its image pull credentials
come from `registry-hatchk8s`. The tunnel reads `cloudflare-tunnel` in
`civpulse-edge`. Collector and self-hosted Supabase secrets remain scoped to
their environment namespaces. Migration preserved their values without rotation.

## Capacity and availability

Thor is a single-node cluster. Its local storage and dev/test services depend on
that host's availability. Separate namespaces and databases isolate environment
configuration and data; they do not make Thor highly available.

Production uses one DigitalOcean pool of three
`s5-2vcpu-2gb-50gb` shared workers, with autoscaling minimum 3 and maximum 4.
API, tunnel, and Traefik each have two replicas, required hostname anti-affinity,
and disruption budgets keeping at least one replica available during voluntary
disruptions. The collector and monitor are single-replica workloads and can
restart during node maintenance.

At the recorded worker rates, the baseline is $103.95/month; four workers cost
$138.60/month. These amounts exclude retained storage, control plane, traffic,
and taxes. The temporary dedicated `civpulse-api` pool was removed, reducing the
worker baseline by $58.79/month. Shared 4-GB and 6-GB replacement pools were
unavailable in the region during consolidation.

Production resource reservations fit on two workers after losing one; this is
not a peak-load guarantee. Regional capacity shortages may also prevent an
autoscaler expansion. See the [capacity receipt](k8s/capacity.json) for pool
configuration and verification evidence.

## Operations and deployment changes

Use explicit contexts for routine checks:

```sh
kubectl --context thor -n civpulse-dev get deployments,statefulsets,pvc
kubectl --context thor -n civpulse-test get deployments,statefulsets,pvc
helm --kube-context thor list -n civpulse-dev
helm --kube-context thor list -n civpulse-test

kubectl --context do-atl1-k8s-1-37-0-do-2-atl1-1793724241236 -n civpulse-prod get deployments
kubectl --context do-atl1-k8s-1-37-0-do-2-atl1-1793724241236 -n civpulse-edge get deployments,services
```

Deploy dev/test collector and Supabase changes to Thor using their corresponding
environment values and existing secrets. The nonproduction Supabase bootstrap is
restricted to context `thor` and uses `local-path` storage. Deploy production
collector and API changes to the DigitalOcean context. Preserve the live
collection, monitoring, and notification settings during upgrades; do not replace
them with disabled scaffold defaults.

Thor's `civpulse-dev` namespace also contains pre-existing applications. Operate
on the named Helm releases or workloads rather than deleting the namespace or
scaling everything in it.

Publish immutable image digests before applying a production change. After an
upgrade, check rollout status, collector database health, and the API using both
missing credentials (expected `401`) and an authorized user's token. TCP probes
and collector heartbeat probes establish process availability, not remote
database health.

## Migration and rollback

Dev/test moved from DigitalOcean to Thor on 2026-11-04. Writers were stopped,
PostgreSQL shut down cleanly, and database, key, and Storage volumes copied.
All persistent-file checksums matched before startup. Both migrated collectors
passed authenticated database health checks, and both databases retained all
139 recorded migrations. Details are in the
[migration receipt](thor/nonprod-migration.json).

The original DigitalOcean PVCs are retained as migration-time rollback copies.
They are not continuously synchronized backups. Keep their source workloads
stopped while Thor is active.

To roll back, stop the corresponding Thor writers first. If writes made after
the migration must be preserved, copy that current state back before restoring
the source replica counts recorded in the receipt. Otherwise, restoring the old
volumes restores the migration-time state. Never run both copies of an
environment concurrently. Deleting retained PVCs is a separate cleanup decision.
