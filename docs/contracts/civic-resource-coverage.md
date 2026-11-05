# Civic resource coverage

Reviewed against 81 live public tables plus the county-statistic projection. Direct operations remain subject to caller RLS. Core discovery/read use existing visibility-aware projections; admin-only raw reads have an explicit admin preflight. Workflow-only evidence retains existing immutability and ownership rules.

| Resource | Direct table operations | Raw read gate | Guarded workflows |
|---|---|---|---|
| article_bills | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| article_organizations | read, create, update, delete | Admin | read_related_records, save_relationship, delete_relationship |
| article_people | read, create, update, delete | Admin | read_related_records, save_relationship, delete_relationship |
| articles | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| ballot_questions | read, create, update, delete | Admin | search_site, get_civic_record |
| bill_actions | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| bill_amendments | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| bill_positions | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| bill_relationships | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| bill_sponsorships | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| bill_versions | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| bills | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| calendars | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| contest_candidates | read, create, update, delete | Admin | read_related_records, save_relationship, delete_relationship |
| contests | read, create, update, delete | Admin | search_site, get_civic_record |
| county_geographies | read, create, update, delete | Existing RLS/workflow |  |
| county_statistic_indicators | read, create, update, delete | Existing RLS/workflow |  |
| county_statistic_sources | read, create, update, delete | Existing RLS/workflow |  |
| county_statistic_topics | read, create, update, delete | Existing RLS/workflow |  |
| county_statistics | read, create, update, delete | Existing RLS/workflow |  |
| election_result_reports | read | Admin | manage_election_results |
| elections | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| events | read, create, update, delete | Admin | search_site, get_civic_record |
| georgia_turnout_counties | Workflow only | Existing RLS/workflow | get_county_statistic_catalog |
| layer_features | read, create, update, delete | Admin | search_layer_features, get_layer_feature, save_layer_feature, delete_layer_feature |
| layers | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| legislative_bodies | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_body_memberships | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_event_links | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_sessions | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_terms | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_vote_bills | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_vote_choices | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| legislative_votes | read, create, update, delete | Admin | browse_legislation, describe_legislative_resource, save_legislative_record, delete_legislative_record |
| map_layers | read, create, update, delete | Admin | read_related_records, save_relationship, delete_relationship |
| maps | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| meeting_attachments | read, create, update, delete | Admin | get_meeting, import_meeting_records |
| meeting_attendance | read, create, update, delete | Admin | get_meeting, import_meeting_records |
| meeting_record_sources | read | Admin | import_meeting_records |
| meeting_vote_choices | read, create, update, delete | Admin | get_meeting, import_meeting_records |
| meeting_vote_outcome_observations | read | Admin | set_meeting_vote_outcome, get_person_meeting_votes |
| meeting_votes | read, create, update, delete | Admin | get_meeting, import_meeting_records |
| obligation_observations | read | Admin | browse_obligations, accept_obligation_report |
| obligation_reports | read | Admin | browse_obligations, accept_obligation_report |
| office_terms | read, create, update, delete | Admin | read_related_records, save_office_term, delete_civic_data |
| officeholder_observations | read | Admin | read_officeholder_observations, accept_officeholder_observations |
| offices | read, create, update, delete | Existing RLS/workflow | list_offices, save_office, delete_civic_data |
| organization_relationships | read, create, update, delete | Existing RLS/workflow | read_related_records, save_relationship, delete_relationship |
| organizations | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| people | read, create, update, delete | Existing RLS/workflow | search_site, get_civic_record |
| person_organizations | read, create, update, delete | Existing RLS/workflow | read_related_records, save_relationship, delete_relationship |
| person_relationships | read, create, update, delete | Existing RLS/workflow | read_related_records, save_relationship, delete_relationship |
| profiles | read, create, update | Existing RLS/workflow | get_my_profile, update_my_profile |
| public_document_meetings | Workflow only | Existing RLS/workflow | get_public_document, link_public_document_meeting |
| public_documents | Workflow only | Existing RLS/workflow | list_public_documents, get_public_document, publish_meeting_document, withdraw_public_document |
| records_request_fees | Workflow only | Existing RLS/workflow | get_records_request, add_records_request_item, remove_records_request_item |
| records_request_files | Workflow only | Existing RLS/workflow | get_records_request, add_records_request_item, remove_records_request_item, manage_records_request_file |
| records_request_links | Workflow only | Existing RLS/workflow | get_records_request, add_records_request_item, remove_records_request_item |
| records_request_owners | Workflow only | Existing RLS/workflow | get_records_request, transfer_records_request |
| records_request_transfers | Workflow only | Existing RLS/workflow | get_records_request, transfer_records_request |
| records_request_updates | Workflow only | Existing RLS/workflow | get_records_request, add_records_request_item, remove_records_request_item |
| records_requests | Workflow only | Existing RLS/workflow | search_records_requests, get_records_request, save_records_request, transfer_records_request |
| school_finance_recipients | Workflow only | Existing RLS/workflow | school_finances |
| school_finance_releases | Workflow only | Existing RLS/workflow | school_finances |
| school_finance_rows | Workflow only | Existing RLS/workflow | school_finances |
| short_link_stats | read | Admin | search_civic_data |
| short_links | read | Admin | save_short_link |
| sos_ballot_links | Workflow only | Existing RLS/workflow | get_sos_results_mappings, put_sos_results_mapping, delete_sos_results_mapping |
| sos_result_items | read | Admin | manage_election_results, read_official_election_results, get_sos_results, begin_sos_results, stage_sos_results, publish_sos_results |
| sos_result_snapshots | read | Admin | manage_election_results, read_official_election_results, get_sos_results, begin_sos_results, stage_sos_results, publish_sos_results |
| structured_turnout_reports | read | Admin | manage_election_results |
| user_fee_observations | read | Admin | browse_user_fees, accept_user_fee_report |
| user_fee_reports | read | Admin | browse_user_fees, accept_user_fee_report |
| user_fees | read | Admin | browse_user_fees, accept_user_fee_report |
| voter_roll_imports | read | Admin | begin_voter_roll_import, append_voter_roll_records, complete_voter_roll_import, get_voter_roll_history |
| voter_roll_records | read | Admin | get_voter_roll_history, append_voter_roll_records |

Internal state is not a civic CRUD surface:

- `admins`: Account role membership is managed through existing administrator tools.
- `frontend_rebuild_state`: Internal frontend build coordination.
- `sos_automation_feeds`: Internal collector scheduling and lease state.
- `sos_automation_jobs`: Internal collector jobs and lease state.
- `meeting_upload_sessions`: Durable upload capabilities are available only through guarded upload tools.

`county_statistic_records` is a derived projection accessed through `search_county_statistics`; it has no independent writes.
