# Official election result reads

Authenticated callers can use either service to read current reported Georgia
results without administrator access or a database import:

- REST: `GET /api/v1/elections/{election_id}/results?query=DeFranco`
- MCP: `read_official_election_results({"election_id":"295d864d-80c8-4a87-ab16-3f4f3fa5ddb7","query":"McLaurin"})`

Find IDs with MCP `search_site` (type `election`) or
`GET /api/v1/elections?query=November`. REST also supports
`GET /api/v1/people?query=DeFranco`. Searches return at most 100 visible rows,
ordered by ID; query is an optional case-insensitive substring. People and
elections remain protected by existing database RLS. Result reads resolve the
existing visibility-aware `get_election` RPC with the caller's bearer token.

The shared `civpulse-election-results` crate implements both result surfaces.
Only the election's configured HTTPS `results.sos.ga.gov` Georgia
`export-MMDDYYYYGeneralAndSpecialElection.json` URL is supported, and its date
must agree with the election. There are no redirects, caller-selected hosts,
service-role credentials or database writes. The public source client receives
no caller credentials. Other source formats fail closed.

`query` matches a candidate or contest name (case-insensitive substring, max
200 bytes), preserving all choices in each matching contest. Empty query returns
all statewide contests; empty matches are not zero votes. Results include exact
integer counts, approximate choice-vote percentages, voting-method breakdowns,
source choice and contest IDs, source reporting statuses, nullable precinct counts,
source timestamp, retrieval timestamp and exact-byte SHA-256. County scopes are
never added to statewide totals. Null counts stay unknown; incomplete contests
have no computed total or percentages. Vote shares use choice votes, not ballots
cast; this distinction matters for multi-seat races. Source identities are not
asserted to be Civic Pulse person/contest identities.

Certification is `not_established`, even when vote counts are present. Reporting
statuses are preserved independently: nonzero totals do not imply all precincts
reported. Compare counts to describe a lead, not a certified winner.

Source refreshes have a 60-second deadline and 64 MiB limit. Each process serializes
refreshes and caches one public snapshot for 60 seconds; stale data is not returned
on refresh failure. County detail is skipped during deserialization, and hashing
and parsing run on a blocking worker. All authenticated responses remain
`Cache-Control: no-store`. Unavailable/inaccessible elections and unsupported or
failed feeds produce a sanitized API 502 or an MCP tool error, never zero totals.

The frozen upstream MCP catalog is retained; native tools live separately in
`crates/civpulse-civic/reference/extensions.json` so regenerating the upstream oracle cannot
overwrite them.
