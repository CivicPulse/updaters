# Isolated Supabase for development and tests

Use the Supabase-docs-linked [community Helm chart](https://github.com/supabase-community/supabase-kubernetes),
version **0.8.0**, separately in `civpulse-dev` and `civpulse-test` on context
`do-atl1-k8s-1-37-0-do-2-atl1-1793724241236`. Production continues to use the existing
website Supabase deployment. No production database migration is applied here.
CloudNativePG is unnecessary for this stack: the chart's Supabase Postgres image
provides the platform initialization and extension support the contracts need.

`chart.lock.json` pins the exact published chart archive and checksum.
`migrations.lock.json` pins all 137 website migration files at reviewed commit
`52b32714b5c5ce633060381ee16fdde2b063341d`. This includes prerequisite application
schemas as well as the collector queue, fencing, publication guards, mapping
contracts and reviewed hourly-to-five-minute cadence; there is no competing worker database model.

The lean stack enables Postgres, Auth, Storage, PostgREST and Kong. Auth/Storage
create schemas referenced by application migrations. Studio, Meta, Realtime,
Imgproxy, MinIO, Functions, analytics and Vector are disabled. Storage here serves
only the isolated application contract; public election evidence still uses the
separate R2 environment bucket. The edge coordinator can be added for the later
end-to-end scheduling acceptance; it is not implied by this base installation.

Postgres is pinned to the verified `17.11.0.003` image digest to include the
[current timezone fixes](https://supabase.com/changelog/tzdata-local-time-vancouver-edmonton-casablanca).
The chart's default older database tag is overridden. Data, pgsodium keys and local
Storage data use separate persistent volumes. Do not delete them to recover an
application import. The upstream Postgres entrypoint initializes filesystem
ownership as root and drops to the database account; its limited bootstrap
capabilities are documented in values. An additional pinned init container fills missing configuration defaults after the
upstream initializer: fresh DigitalOcean block volumes contain `lost+found`, which
causes upstream’s empty-directory guard to skip its copy. The additional copy never
overwrites existing files or symlinks, including pgsodium keys or customized
configuration. It walks missing entries recursively because the pinned image’s
BusyBox `cp -n` skips an already-existing target directory as a whole. Helm CI
executes the actual init command inside that exact database image, checking fresh
`lost+found` volumes, missing nested defaults, and preservation of existing keys
and custom settings. The additional init changes only the mount root directory's
owner to `postgres:postgres`, retaining its mode and all existing file ownership,
so the pinned image can create its persistent `pgsodium_root.key`. CI also starts
and restarts the real database image with representative root-owned volume
permissions and the deployment's restricted capability set; key output is never
printed.
Collector pods use the stricter non-root,
read-only-root policy. All services are internal ClusterIP with no Ingress.

Storage and REST use single-replica `Recreate` upgrades. Storage's ReadWriteOnce
volume must detach from the old pod before a replacement starts; REST also avoids
a temporary second memory request on the shared small nodes. These upgrades have
brief API downtime. Auth/Kong and the ordered database StatefulSet retain their
upstream rollout behavior. The pinned community chart has no Storage/REST strategy
value. Bootstrap verifies the upstream archive SHA, then derives a temporary
archive with exactly two reviewed template insertions. Original and patched
Storage/REST template hashes and unique anchors are checked; every other member's
bytes remain unchanged. The patch adds `Recreate` with `rollingUpdate: null` to
clear previous surge defaults during upgrades. It is compatible with Helm 3 and
4 without plugins. An upstream chart update requires reviewing these hashes and
insertions again; an unexpected archive/template fails before cluster writes.

## Review and bootstrap

```sh
# Read-only: no cluster calls or writes.
python3 deploy/supabase/bootstrap.py dev
python3 deploy/supabase/bootstrap.py test

# Run only after the deployment/migration review and environment acceptance.
python3 deploy/supabase/bootstrap.py dev --apply
python3 deploy/supabase/bootstrap.py test --apply
```

The bootstrap accepts only dev/test and the exact configured context. It verifies
the context's cluster mapping, namespace, chart checksum and every SQL checksum.
It generates distinct per-environment cryptographic secrets directly into
Kubernetes; no credentials appear in argv, Helm values, source, or routine output.
Existing secrets are reused without rotation, and incomplete existing secrets
fail closed. `results-supabase` receives the environment's service-role token and
exact internal gateway URL:

```text
http://supabase-supabase-kong.civpulse-dev.svc.cluster.local:8000
http://supabase-supabase-kong.civpulse-test.svc.cluster.local:8000
```

The executable uses the environment-bound RPC constructor: it permits these
exact internal HTTP URLs only in the matching nonproduction environment. Wrong
namespaces, arbitrary internal hostnames, ports, credential-bearing URLs, paths,
queries and fragments fail closed. Production requires HTTPS, including health
mode. The offline simulator retains its explicit loopback test adapter. The worker calls the same
`/rest/v1/rpc/*` API and role checks in every environment; it never writes database
tables directly.

After chart readiness and Auth/Storage schema checks, bootstrap enables standard
Supabase baseline pgcrypto/Vault extensions and replays the pinned application
migrations in filename order. Each file and its checksum receipt commit in one
transaction. The local `civpulse_dev_bootstrap.migrations` ledger permits safe
restart and rejects changed applied files. The 4.8MB migration set is downloaded
and verified into a private temporary directory rather than placed in an
oversized ConfigMap. The temporary directory is removed when bootstrap finishes.

Application migrations install cron routines, but bootstrap deliberately creates
no external poll/rebuild Vault endpoint or token, leaves all feeds disabled, and
checks those invariants before reporting success. It does not copy production
users, private records, tokens, cron secrets, or editorial data. Any fixtures and
reviewed test election records are a later explicit acceptance step.

## Release verification

Confirm Auth and Storage schemas, PostGIS, pg_cron, pg_net and Vault are present;
call `get_sos_collection_health` using the generated service-role credential;
verify anonymous callers cannot claim jobs; then run queue/publication recovery
checks with isolated fixtures and R2 credentials. Run both environment bootstraps
and record their actual outcomes. A successful template render or dry run does
not claim live database acceptance.

The collector chart's egress policy permits same-namespace gateway access only.
The database policy permits same-namespace clients. Before exposure outside the
cluster, review additional gateway/default-deny network policy and TLS; this
installation intentionally exposes no external endpoint. Public R2 dev/test
origins are separately provisioned and must never contain private data.

The bootstrap does not set alert routes, run the edge polling function, activate
production collection, or authorize evidence deletion. Follow the feature's
operational acceptance/runbook before any of those actions.

## Local replay evidence

`replay-evidence.json` records a fresh replay of all 136 checksum-verified SQL
files from the pinned merged website commit against the exact database image.
The chart's pinned Auth and Storage versions started as UID 1000 before replay.
The checks found PostGIS, pg_cron, pg_net and Vault, exactly one coordinator
schedule, zero enabled feeds, zero external poll/rebuild Vault secrets, an empty
HTTP queue and an empty operational health list. Active chart images and the
PostgreSQL init client are pinned by registry-verified digests.

This is local SQL/platform-image evidence. It does not claim a Helm installation,
Kong/PostgREST runtime acceptance, Kubernetes rollout, or production acceptance.
Those checks remain required after the chart review. The collector golden oracle
remains pinned independently to website commit `0408add4c47a8549c9ad76c680d9a5779a58a050`;
updating the database migration lock does not regenerate or rewrite source goldens.
