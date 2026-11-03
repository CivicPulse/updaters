# Deployment environments

All environments use `do-atl1-k8s-1-37-0-do-2-atl1-1793724241236`. Namespace names
are `civpulse-dev`, `civpulse-test`, and `civpulse-prod`. Each committed collector
values file disables collection and leaves production acceptance unapproved.

Use the [collector chart](../helm/civpulse-results-collector/README.md) with a
scanned DOCR manifest digest. Use [self-hosted Supabase](../supabase/README.md) only
for dev/test. Keep database and R2 secrets separate across namespaces. Merely
sharing a Kubernetes host never authorizes dev/test writes to production.

The main-only publish workflow builds and scans the collector image, then pushes
`registry.digitalocean.com/hatchk8s/civpulse-results-collector:sha-<commit>` using the
`DOCR_CONFIG` GitHub secret. It emits the immutable manifest reference in the run
summary. It deploys nothing and enables no collection. Installation always uses
the emitted digest, never a mutable tag.
