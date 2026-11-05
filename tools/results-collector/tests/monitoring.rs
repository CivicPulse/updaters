use chrono::{DateTime, Duration, Utc};
use civpulse_results_collector::monitoring::{
    self, AlertKind, HealthRow, ObservationState, PointerObservation,
};
use civpulse_results_contract::Revision;
use serde_json::json;
use uuid::Uuid;
const ELECTION: &str = "295d864d-80c8-4a87-ab16-3f4f3fa5ddb7";
fn now() -> DateTime<Utc> {
    "2026-11-03T12:00:00Z".parse().unwrap()
}
fn revision(value: &str) -> Revision {
    Revision::try_from(value.to_owned()).unwrap()
}
fn row(time: DateTime<Utc>) -> HealthRow {
    let mut row:HealthRow=serde_json::from_value(json!({"election_id":ELECTION,"enabled":true,"last_attempt_at":time,"last_success_at":time,"last_error":null,"current_revision":"9007199254740993","job_id":null,"job_state":null,"job_attempts":null,"job_updated_at":null})).unwrap();
    row.checks_expected = true;
    row
}
fn matching() -> PointerObservation {
    PointerObservation::Present {
        revision: revision("9007199254740993"),
    }
}
fn has(report: &monitoring::Report, kind: AlertKind) -> bool {
    report.alerts.iter().any(|alert| alert.kind == kind)
}
#[test]
fn unchanged_source_for_hours_is_healthy_when_checks_succeed() {
    let mut state = ObservationState::default();
    for hour in 0..8 {
        let time = now() + Duration::hours(hour);
        let report = monitoring::evaluate(vec![(row(time), matching())], time, &mut state).unwrap();
        assert!(report.alerts.is_empty());
    }
}
#[test]
fn exact_revision_divergence_persists_for_grace_and_clears_after_repair() {
    let mut state = ObservationState::default();
    let older = || PointerObservation::Present {
        revision: revision("9007199254740992"),
    };
    let first = monitoring::evaluate(vec![(row(now()), older())], now(), &mut state).unwrap();
    assert!(first.alerts.is_empty());
    let later = now() + Duration::minutes(16);
    let report = monitoring::evaluate(vec![(row(later), older())], later, &mut state).unwrap();
    assert!(has(&report, AlertKind::PointerDivergence));
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("9007199254740993"));
    assert!(json.contains("9007199254740992"));
    let repaired = monitoring::evaluate(vec![(row(later), matching())], later, &mut state).unwrap();
    assert!(!has(&repaired, AlertKind::PointerDivergence));
    assert!(
        state.feeds[&Uuid::parse_str(ELECTION).unwrap()]
            .mismatch_since
            .is_none()
    );
}
#[test]
fn stalled_job_is_detected_despite_fresh_lease_renewal_and_new_job_resets_age() {
    let mut state = ObservationState::default();
    let job = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    let pending = |time| {
        let mut row = row(time);
        row.job_id = Some(job);
        row.job_state = Some("importing".into());
        row.job_updated_at = Some(time);
        row
    };
    monitoring::evaluate(vec![(pending(now()), matching())], now(), &mut state).unwrap();
    let time = now() + Duration::minutes(16);
    let report = monitoring::evaluate(vec![(pending(time), matching())], time, &mut state).unwrap();
    assert!(has(&report, AlertKind::PublicationBacklog));
    let mut new = pending(time);
    new.job_id = Some(Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap());
    let report = monitoring::evaluate(vec![(new, matching())], time, &mut state).unwrap();
    assert!(!has(&report, AlertKind::PublicationBacklog));
}
#[test]
fn missing_initial_checks_alert_only_after_active_window_grace() {
    let mut state = ObservationState::default();
    let unchecked = || {
        let mut row = row(now());
        row.last_attempt_at = None;
        row.last_success_at = None;
        row
    };
    let first = monitoring::evaluate(vec![(unchecked(), matching())], now(), &mut state).unwrap();
    assert!(first.alerts.is_empty());
    let time = now() + Duration::minutes(16);
    let report = monitoring::evaluate(vec![(unchecked(), matching())], time, &mut state).unwrap();
    assert!(has(&report, AlertKind::StaleSourceChecks));
    assert!(has(&report, AlertKind::StaleSuccessfulChecks));
    let mut paused = unchecked();
    paused.checks_expected = false;
    let paused = monitoring::evaluate(vec![(paused, matching())], time, &mut state).unwrap();
    assert!(!has(&paused, AlertKind::StaleSourceChecks));
}
#[test]
fn retries_quarantine_and_unavailable_pointer_are_actionable_without_leaking_errors() {
    let mut row = row(now());
    row.job_id = Some(Uuid::nil());
    row.job_state = Some("queued".into());
    row.job_attempts = Some(8);
    row.last_error = Some("secret-token https://private.example/source-payload".into());
    let report = monitoring::evaluate(
        vec![(row, PointerObservation::Unavailable)],
        now(),
        &mut ObservationState::default(),
    )
    .unwrap();
    for kind in [
        AlertKind::RepeatedErrors,
        AlertKind::QuarantinedJob,
        AlertKind::PointerUnavailable,
    ] {
        assert!(has(&report, kind));
    }
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("secret-token"));
    assert!(!json.contains("private.example"));
    assert!(!json.contains("source-payload"));
    let metrics = monitoring::prometheus(&report);
    assert!(metrics.contains("kind=\"quarantined_job\""));
    assert!(metrics.contains("civpulse_sos_pointer_matches_database"));
    assert!(!metrics.contains("9007199254740993"));
    assert!(!metrics.contains("private.example"));
}
#[test]
fn duplicate_unknown_state_and_backward_observation_clock_fail_closed() {
    assert!(
        monitoring::evaluate(
            vec![(row(now()), matching()), (row(now()), matching())],
            now(),
            &mut ObservationState::default()
        )
        .is_err()
    );
    let mut invalid = row(now());
    invalid.job_state = Some("private payload".into());
    assert!(
        monitoring::evaluate(
            vec![(invalid, matching())],
            now(),
            &mut ObservationState::default()
        )
        .is_err()
    );
    let mut state = ObservationState::default();
    let mut pending = row(now());
    pending.job_id = Some(Uuid::nil());
    pending.job_state = Some("queued".into());
    monitoring::evaluate(vec![(pending, matching())], now(), &mut state).unwrap();
    let mut pending = row(now());
    pending.job_id = Some(Uuid::nil());
    pending.job_state = Some("queued".into());
    assert!(
        monitoring::evaluate(
            vec![(pending, matching())],
            now() - Duration::minutes(1),
            &mut state
        )
        .is_err()
    );
}
#[tokio::test]
async fn state_round_trip_missing_corruption_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut state = monitoring::load_state(&path).await.unwrap();
    monitoring::evaluate(
        vec![(row(now()), PointerObservation::Missing)],
        now(),
        &mut state,
    )
    .unwrap();
    monitoring::save_state(&path, &state).await.unwrap();
    let loaded = monitoring::load_state(&path).await.unwrap();
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        serde_json::to_value(loaded).unwrap()
    );
    tokio::fs::write(&path, b"truncated").await.unwrap();
    assert!(monitoring::load_state(&path).await.is_err());
    tokio::fs::write(&path, vec![b'x'; 1024 * 1024 + 1])
        .await
        .unwrap();
    assert!(monitoring::load_state(&path).await.is_err());
}
#[tokio::test]
async fn collect_reads_only_operational_rpcs_and_consistent_s3() {
    use async_trait::async_trait;
    use civpulse_results_collector::{
        Error, Result,
        rpc::Rpc,
        storage::{Conditions, Store, Stored},
    };
    use std::{
        path::Path,
        sync::{Arc, Mutex},
        time::Duration as StdDuration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    struct S3 {
        keys: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl Store for S3 {
        fn url(&self, _: &str) -> Result<String> {
            panic!("CDN/public URL must never be used")
        }
        async fn get(&self, key: &str, _: usize) -> Result<Stored> {
            self.keys.lock().unwrap().push(key.into());
            Ok(Stored{status:200,etag:Some("etag".into()),body:json!({"schema_version":1,"election_id":ELECTION,"snapshot_id":"00000000-0000-4000-8000-000000000002","revision":"9007199254740993","fingerprint":"a".repeat(64)}).to_string().into_bytes()})
        }
        async fn put(&self, _: &str, _: &[u8], _: Conditions) -> Result<u16> {
            panic!("monitor attempted PUT")
        }
        async fn put_file(&self, _: &str, _: &Path, _: &str, _: u64, _: Conditions) -> Result<u16> {
            panic!("monitor attempted file PUT")
        }
        async fn download(&self, _: &str, _: &Path, _: &str, _: u64) -> Result<()> {
            Err(Error::Unsupported)
        }
        async fn verify(&self, _: &str, _: &str, _: u64) -> Result<()> {
            Err(Error::Unsupported)
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(vec![]));
    let seen = routes.clone();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);
            let route = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_owned();
            seen.lock().unwrap().push(route.clone());
            let body = if route.ends_with("get_sos_collection_health") {
                json!([{"election_id":ELECTION,"enabled":true,"last_attempt_at":"2026-11-03T01:00:00Z","last_success_at":"2026-11-03T01:00:00Z","last_error":null,"current_revision":"9007199254740993","job_id":null,"job_state":null,"job_attempts":null,"job_updated_at":null}])
            } else if route.ends_with("get_sos_results_health") {
                json!({"status":"paused"})
            } else {
                panic!("unexpected mutation {route}")
            };
            let body = body.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let rpc = Rpc::new(&url, "test-key", StdDuration::from_secs(2), 4096).unwrap();
    let store = S3 {
        keys: Mutex::new(vec![]),
    };
    let report = monitoring::collect(&rpc, &store, now(), &mut ObservationState::default())
        .await
        .unwrap();
    assert!(report.alerts.is_empty());
    assert!(!report.feeds[0].checks_expected);
    assert_eq!(
        store.keys.lock().unwrap()[0],
        format!("elections/{ELECTION}/latest.json")
    );
    server.await.unwrap();
    assert_eq!(routes.lock().unwrap().len(), 2);
    assert_eq!(
        report.feeds[0].publication_fingerprint,
        Some("a".repeat(64))
    );
}
#[test]
fn hourly_cadence_uses_two_hours_plus_grace_then_transitions_to_five_minutes() {
    let mut state = ObservationState::default();
    let mut hourly = row(now());
    hourly.poll_interval_seconds = Some(3600);
    hourly.last_attempt_at = Some(now() - Duration::minutes(90));
    hourly.last_success_at = hourly.last_attempt_at;
    let report = monitoring::evaluate(vec![(hourly, matching())], now(), &mut state).unwrap();
    assert!(report.alerts.is_empty());
    assert_eq!(report.feeds[0].source_check_stale_after_seconds, 7500);
    let mut five = row(now());
    five.poll_interval_seconds = Some(300);
    five.last_attempt_at = Some(now() - Duration::minutes(16));
    five.last_success_at = five.last_attempt_at;
    let report = monitoring::evaluate(vec![(five, matching())], now(), &mut state).unwrap();
    assert!(has(&report, AlertKind::StaleSourceChecks));
    assert_eq!(report.feeds[0].source_check_stale_after_seconds, 900);
    let at: DateTime<Utc> = "2026-11-03T22:00:00Z".parse().unwrap();
    assert_eq!(
        monitoring::effective_cadence(
            Some("3600"),
            Some("2026-11-03T22:00:00Z"),
            Some("300"),
            at - Duration::seconds(1)
        )
        .unwrap(),
        3600
    );
    assert_eq!(
        monitoring::effective_cadence(Some("3600"), Some("2026-11-03T22:00:00Z"), Some("300"), at)
            .unwrap(),
        300
    );
    assert!(monitoring::effective_cadence(Some("0"), None, Some("300"), at).is_err());
}
#[test]
fn cadence_transition_anchor_allows_first_new_phase_check_without_forging_raw_provenance() {
    let transition: DateTime<Utc> = "2026-11-03T22:00:00Z".parse().unwrap();
    let mut phase = row(transition);
    phase.poll_interval_seconds = Some(300);
    phase.source_check_expected_since = Some(transition);
    phase.last_attempt_at = Some(transition - Duration::hours(1));
    phase.last_success_at = phase.last_attempt_at;
    let report = monitoring::evaluate(
        vec![(phase, matching())],
        transition + Duration::minutes(1),
        &mut ObservationState::default(),
    )
    .unwrap();
    assert!(report.alerts.is_empty());
    assert_eq!(report.feeds[0].source_check_age_seconds, Some(60));
}

#[test]
fn actual_job_creation_detects_backlog_with_fresh_renewal_under_hourly_cadence() {
    {
        let mut row = row(now());
        row.poll_interval_seconds = Some(3600);
        row.last_attempt_at = Some(now() - Duration::minutes(40));
        row.last_success_at = row.last_attempt_at;
        row.job_id = Some(Uuid::nil());
        row.job_state = Some("importing".into());
        row.job_updated_at = Some(now());
        row.job_created_at = Some(now() - Duration::minutes(20));
        let report = monitoring::evaluate(
            vec![(row, matching())],
            now(),
            &mut ObservationState::default(),
        )
        .unwrap();
        assert!(has(&report, AlertKind::PublicationBacklog));
        assert!(!has(&report, AlertKind::StalledJob));
        assert!(!has(&report, AlertKind::StaleSourceChecks));
    }
    let mut row = row(now());
    row.poll_interval_seconds = Some(3600);
    row.last_attempt_at = Some(now() - Duration::minutes(40));
    row.last_success_at = row.last_attempt_at;
    row.job_id = Some(Uuid::nil());
    row.job_state = Some("importing".into());
    row.job_updated_at = Some(now());
    row.job_created_at = Some(now() - Duration::minutes(2));
    let report = monitoring::evaluate(
        vec![(row, matching())],
        now(),
        &mut ObservationState::default(),
    )
    .unwrap();
    assert!(report.alerts.is_empty());
}

#[test]
fn old_official_source_generation_time_alone_never_sends_an_alert() {
    let mut row = row(now());
    row.source_to_publication_lag_seconds = Some(28800);
    row.ingestion_lag_seconds = Some(5);
    let report = monitoring::evaluate(
        vec![(row, matching())],
        now(),
        &mut ObservationState::default(),
    )
    .unwrap();
    assert!(report.alerts.is_empty());
    assert_eq!(
        report.feeds[0].source_to_publication_lag_seconds,
        Some(28800)
    );
}
