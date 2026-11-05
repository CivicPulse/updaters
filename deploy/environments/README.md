# Deployment environments

Dev and test use context `thor`. Production uses
`do-atl1-k8s-1-37-0-do-2-atl1-1793724241236`. Namespace names are `civpulse-dev`, `civpulse-test`, and `civpulse-prod`. Each committed collector
values file disables collection and leaves production acceptance unapproved.

Use the [collector chart](../helm/civpulse-results-collector/README.md) with a
scanned DOCR manifest digest. Use [self-hosted Supabase](../supabase/README.md) only
for dev/test. Keep database and R2 secrets separate across namespaces. Merely
sharing a Kubernetes host never authorizes dev/test writes to production.

The main-only publish workflow builds and scans all four tool images, including
the collector, then pushes
`registry.digitalocean.com/hatchk8s/civpulse-results-collector:sha-<commit>` using the
`DOCR_CONFIG` GitHub secret. It emits the immutable manifest reference in the run
summary. It deploys nothing and enables no collection. Installation always uses
the emitted digest, never a mutable tag.

Production values enable ntfy at the approved public `https://ntfy.sh/CivPulse`
topic when the existing monitor is enabled. Dev/test and chart defaults keep it
disabled. Email remains separately controlled; preserve the current release's
active collection/monitor/email flags during upgrades rather than applying the
committed disabled-collection defaults directly to a live release.
