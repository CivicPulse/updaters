# Results notifications

The existing minute monitor can publish to the public `https://ntfy.sh/CivPulse`
topic in addition to its existing email channel. There is no new source scheduler.
Chart defaults disable push; production values opt in when monitoring is enabled.
`SOS_NTFY_ENABLED=true`, `CIVPULSE_ENV=prod`, and the exact
`SOS_NTFY_URL=https://ntfy.sh/CivPulse` are required. No provider credential is
required. The endpoint cannot be redirected or overridden to another topic.

Messages contain only safe actionable condition labels for the reviewed public
November election or deployment monitor, or a link to its public election page.
Unknown/private feed UUIDs, names, source URLs, vote data, error bodies and secrets
are excluded. Email retains its existing private operational error/recovery route.

The first valid confirmed publication silently establishes a baseline, including
an already-published revision on installation. Missing/malformed fingerprints,
pending work and disagreement between the database and the signed R2 pointer
cannot seed a baseline or send new-data alerts. Later strictly greater confirmed
revisions notify only if the semantic fingerprint changes. Formatting/time-only
revisions quietly advance the watermark; unchanged checks and older observations
stay quiet. Multiple publications between monitor runs coalesce to the latest
confirmed observation; this is not a complete event ledger. Silent bootstrap also
applies after explicit loss/reset of the push state, so retain that PVC.

Error/recovery and new-data delivery markers are independent within
`/state/ntfy.json`; email uses `/state/alerts.json`. Successful delivery persists
even if another message/channel fails. Failed delivery leaves its marker unchanged
and retries at the next scheduled monitor run, without a tight retry loop. A later
state can supersede a failed obsolete alert. A crash after provider acceptance
before state persistence can duplicate a message: delivery is at least once.
Timeouts cover the entire request/body (15 seconds per push), redirects are disabled,
message/response sizes are bounded, and matching provider acknowledgement is
required before advancing a marker. The monitor's 60-second observation budget,
parallel email/push attempts, and at most two sequential 15-second push attempts
fit its existing 120-second Job deadline.

After the reviewed image is scanned/published/deployed, an operator can run an
explicit finite `ntfy-test` command using that same image, with the opt-in variables
above and `SOS_COLLECTOR_ENABLED=false`. It requires no Supabase/R2/email secrets,
changes no feed or notification state, and sends one clearly labelled TEST route
verification. It proves delivery only, never a real error or new publication.
Do not modify persistent deduplication state to manufacture a live event.

Keep email enabled during push failures. Inspect the sanitized `push_delivery`
event and failed monitor Job; avoid printing provider response bodies. Roll back
only `monitoring.ntfyEnabled=false` if needed, preserving active collection,
monitoring, email, identity, resource caps, and the retained state PVC. No actual
notification or deployment is performed by these instructions or the CI tests.
