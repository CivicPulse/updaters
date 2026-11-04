# Reviewed source topology admission

The default remains exact `expectedItems` admission. An optional
`collection.acceptedTopologies` (JSON environment `SOS_ACCEPTED_TOPOLOGIES`)
accepts only explicitly reviewed identity membership hashes instead of exact
item-count equality. Empty/absent means legacy strict counts. At most eight
unique lowercase SHA256 hashes are accepted. Malformed configuration is rejected
before a fresh source request. `expectedJurisdictions`, election date/schema,
unique/nonempty string scope/contest/option IDs, counts, URL, lease, window,
timeouts, download/scope/staging caps and semantic canonical fingerprint remain
unchanged. A matching hash includes exact jurisdiction membership and roles,
contest IDs within each jurisdiction and option IDs within each contest. Thus an
unreviewed removal, addition, substitution or reparenting fails closed even if its
aggregate item count equals a reviewed count. Only the two hashes below are
configured for production; admitting a new structure requires reviewed config.

Encoding is compact UTF8 JSON, with no trailing newline, of an array of
`[role, scope_id, [[item_id, [option_id, ...]], ...]]`. Role is `state` for root
`results`, `county` for each `localResults` entry. Option IDs, item tuples, and
scope tuples are sorted lexicographically by string bytes (IDs remain strings,
including leading zeros). No labels, votes, timestamps or reporting fields enter
this separate hash; the existing canonical semantic fingerprint is unchanged.
JSON strings use standard serde_json escaping. SHA256 covers these encoded bytes.
`validate` prints `topology_sha256` for credential-free offline review.

## November 3 production review

Both raw documents have the same state ID
`09378a07-e6cf-4f66-be7c-ca4aa534f99a` and all159 county IDs. Independent Python
encoding and the Rust parser reproduced these membership hashes:

| Evidence | Source created UTC | Raw SHA256 | Bytes | Items | Topology SHA256 |
| --- | --- | --- | --- | --- | --- |
| Published revision2 | 2026-11-04T00:09:02.7683621Z | `029fd8ba8146f30e86b996e0d95215684535c18e7e83ea62241c0d9208fc78a4` |46514470|4178|`e3a380f76800cae26a72593cc33db5c7f354415c83ab1bff0ea563a27fdefb4b`|
| Bounded official GET, HTTP200 Date2026-11-04T01:07:22Z |2026-11-04T01:05:07.1251539Z|`43bf1170cc832bd504758aac215c0f7712e03d7095d53e3de50f725b8601df2e`|46846807|4171|`ca2594ee8e08dec3c0790cba0c5a51a77e22ecec4febae3634a00ed2bbe2fbb9`|

Official URL:
`https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json`.
Prior immutable archive:
`https://files.civpulse.org/elections/295d864d-80c8-4a87-ab16-3f4f3fa5ddb7/raw/029fd8ba8146f30e86b996e0d95215684535c18e7e83ea62241c0d9208fc78a4.json`.

The official source removed contest `SDA49` (District Attorney–Waycross Judicial
Circuit) statewide and from Bacon003, Brantley013, Charlton024, Coffee034,
Pierce113 and Ware148. Prior candidate39 Marilyn Bennett had4367 votes in the
state/Ware records, zero in the other affected counties. It also removed three
statewide options: SHD39/YashicaMarshall-86- (zero),
SHD134/EugeneAllison-46- (zero), SHD180/DefonsioDaniels-50- (eight votes).
No other scope/contest identity changes were observed. This policy treats the
reviewed official source as authoritative for current projections. It does not
claim the nonzero removals were intentional, correct, or empty cleanup. Earlier
raw archives and published revisions retain the previous evidence; no aggregate
vote monotonicity rule masks official corrections. Unknown future topologies
remain rejected pending review, rather than silently moving an exact count or
broadening a minimum/maximum range.

Offline reproduction with retained public raw files (no provider HTTP required):

```sh
TMPDIR=/path/to/ext4/scratch cargo run --locked -p civpulse-results-source \
  --example validate -- source.json \
  '["e3a380f76800cae26a72593cc33db5c7f354415c83ab1bff0ea563a27fdefb4b","ca2594ee8e08dec3c0790cba0c5a51a77e22ecec4febae3634a00ed2bbe2fbb9"]'
```

The example enforces160 jurisdictions, the selected128MiB download/16MiB scope
limits and the supplied topology allowlist. Large official JSON is not committed;
compact synthetic tests verify admission and rejection, including equal-count
identity mutations and unchanged legacy count defaults.
