//! Read-only remote monitoring; source flatlines are never failure signals.
use crate::{
    Error, Result,
    rpc::Rpc,
    storage::{MAX_POINTER, R2Store, Store},
};
use chrono::{DateTime, Utc};
use civpulse_results_contract::Revision;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    time::Duration,
};
use uuid::Uuid;

const STALLED_SECONDS: i64 = 15 * 60;
const MAX_STATE: usize = 1024 * 1024;

/// Service-only RPC input. Raw error strings are deliberately never serialized.
#[derive(Deserialize)]
pub struct HealthRow {
    pub election_id: Uuid,
    pub enabled: bool,
    #[serde(skip)]
    pub checks_expected: bool,
    #[serde(default)]
    pub source_checks_expected: Option<bool>,
    #[serde(default)]
    pub poll_interval_seconds: Option<u32>,
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub poll_transition_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub next_poll_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source_check_expected_since: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub source_to_publication_lag_seconds: Option<i64>,
    #[serde(skip)]
    pub ingestion_lag_seconds: Option<i64>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub current_revision: Revision,
    pub job_id: Option<Uuid>,
    pub job_state: Option<String>,
    pub job_attempts: Option<u32>,
    #[serde(default)]
    pub job_created_at: Option<DateTime<Utc>>,
    pub job_updated_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PointerObservation {
    Missing,
    Present { revision: Revision },
    Unavailable,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ObservationState {
    #[serde(default)]
    pub feeds: BTreeMap<Uuid, Observed>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Observed {
    pub job_id: Option<Uuid>,
    pub pending_since: Option<DateTime<Utc>>,
    pub mismatch_since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub active_since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub poll_interval_seconds: Option<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    RepeatedErrors,
    QuarantinedJob,
    StalledJob,
    PublicationBacklog,
    StaleSourceChecks,
    StaleSuccessfulChecks,
    PointerDivergence,
    PointerUnavailable,
    MonitoringUnavailable,
}
impl AlertKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RepeatedErrors => "repeated_errors",
            Self::QuarantinedJob => "quarantined_job",
            Self::StalledJob => "stalled_job",
            Self::PublicationBacklog => "publication_backlog",
            Self::StaleSourceChecks => "stale_source_checks",
            Self::StaleSuccessfulChecks => "stale_successful_checks",
            Self::PointerDivergence => "pointer_divergence",
            Self::PointerUnavailable => "pointer_unavailable",
            Self::MonitoringUnavailable => "monitoring_unavailable",
        }
    }
}
#[derive(Serialize)]
pub struct Alert {
    pub election_id: Uuid,
    pub kind: AlertKind,
}
#[derive(Serialize)]
pub struct PollWindow {
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub transition_at: Option<DateTime<Utc>>,
    pub next_poll_at: Option<DateTime<Utc>>,
}
impl From<&HealthRow> for PollWindow {
    fn from(row: &HealthRow) -> Self {
        Self {
            starts_at: row.starts_at,
            ends_at: row.ends_at,
            transition_at: row.poll_transition_at,
            next_poll_at: row.next_poll_at,
        }
    }
}
#[derive(Serialize)]
pub struct FeedStatus {
    pub window: PollWindow,
    pub election_id: Uuid,
    pub enabled: bool,
    pub checks_expected: bool,
    pub poll_interval_seconds: u32,
    pub source_check_stale_after_seconds: i64,
    pub database_revision: Revision,
    pub pointer: PointerObservation,
    pub pending_job: Option<Uuid>,
    pub job_created_at: Option<DateTime<Utc>>,
    pub job_updated_at: Option<DateTime<Utc>>,
    pub job_progress_age_seconds: Option<i64>,
    pub job_attempts: u32,
    pub error_present: bool,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub actual_source_check_age_seconds: Option<i64>,
    pub source_check_age_seconds: Option<i64>,
    pub successful_check_age_seconds: Option<i64>,
    pub pending_age_seconds: Option<i64>,
    pub pointer_mismatch_age_seconds: Option<i64>,
    pub source_to_publication_lag_seconds: Option<i64>,
    pub ingestion_lag_seconds: Option<i64>,
}
#[derive(Serialize)]
pub struct Report {
    pub checked_at: DateTime<Utc>,
    pub feeds: Vec<FeedStatus>,
    pub alerts: Vec<Alert>,
}

/// Evaluate successive operational observations, retaining only identifiers/times locally.
/// # Errors
/// Rejects duplicated elections, unknown durable states and future persisted observations.
pub fn evaluate(
    rows: Vec<(HealthRow, PointerObservation)>,
    now: DateTime<Utc>,
    state: &mut ObservationState,
) -> Result<Report> {
    let mut seen = HashSet::new();
    let mut feeds = Vec::new();
    let mut alerts = Vec::new();
    for (row, pointer) in rows {
        if !seen.insert(row.election_id) {
            return Err(Error::InvalidResponse);
        }
        validate_job_state(row.job_state.as_deref())?;
        let previous = state.feeds.get(&row.election_id);
        let pending = if row.job_state.as_deref() == Some("done") {
            None
        } else {
            row.job_id
        };
        let active_since = if row.enabled && row.checks_expected {
            Some(
                previous
                    .filter(|old| old.poll_interval_seconds == row.poll_interval_seconds)
                    .and_then(|old| old.active_since)
                    .unwrap_or(now),
            )
        } else {
            None
        };
        let pending_since = pending_start(previous, pending, row.job_created_at, now);
        let mismatch = match &pointer {
            PointerObservation::Missing => row.current_revision != revision_zero(),
            PointerObservation::Present { revision } => *revision != row.current_revision,
            PointerObservation::Unavailable => false,
        };
        let mismatch_since = if mismatch {
            Some(previous.and_then(|old| old.mismatch_since).unwrap_or(now))
        } else {
            None
        };
        validate_times([active_since, pending_since, mismatch_since], now)?;
        let anchor = row.source_check_expected_since;
        let source_age = age(anchor.or(row.last_attempt_at), now);
        let success_age = age(
            row.last_success_at.into_iter().chain(active_since).max(),
            now,
        );
        let expected_age = age(active_since, now);
        let pending_age = age(pending_since, now);
        let mismatch_age = age(mismatch_since, now);
        let attempts = row.job_attempts.unwrap_or(0);
        state.feeds.insert(
            row.election_id,
            Observed {
                job_id: pending,
                pending_since,
                mismatch_since,
                active_since,
                poll_interval_seconds: row.poll_interval_seconds,
            },
        );
        let feed = FeedStatus {
            window: PollWindow::from(&row),
            election_id: row.election_id,
            enabled: row.enabled,
            checks_expected: row.checks_expected,
            poll_interval_seconds: row.poll_interval_seconds.unwrap_or(300),
            source_check_stale_after_seconds: check_threshold(
                row.poll_interval_seconds.unwrap_or(300),
            )?,
            database_revision: row.current_revision,
            pointer,
            pending_job: pending,
            job_created_at: row.job_created_at,
            job_updated_at: row.job_updated_at,
            job_progress_age_seconds: age(row.job_updated_at, now),
            job_attempts: attempts,
            error_present: row
                .last_error
                .as_deref()
                .is_some_and(|error| !error.is_empty()),
            last_attempt_at: row.last_attempt_at,
            last_success_at: row.last_success_at,
            actual_source_check_age_seconds: age(row.last_attempt_at, now),
            source_check_age_seconds: source_age,
            successful_check_age_seconds: success_age,
            pending_age_seconds: pending_age,
            pointer_mismatch_age_seconds: mismatch_age,
            source_to_publication_lag_seconds: row.source_to_publication_lag_seconds,
            ingestion_lag_seconds: row.ingestion_lag_seconds,
        };
        append_alerts(
            &feed,
            expected_age,
            age(row.job_updated_at, now),
            &mut alerts,
        );
        feeds.push(feed);
    }
    state.feeds.retain(|id, _| seen.contains(id));
    Ok(Report {
        checked_at: now,
        feeds,
        alerts,
    })
}
fn pending_start(
    previous: Option<&Observed>,
    job: Option<Uuid>,
    created: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    job.map(|id| {
        let observed = previous
            .filter(|old| old.job_id == Some(id))
            .and_then(|old| old.pending_since)
            .unwrap_or(now);
        observed.min(created.map_or(now, |created| created.min(now)))
    })
}
fn validate_times(times: [Option<DateTime<Utc>>; 3], now: DateTime<Utc>) -> Result<()> {
    if times.into_iter().flatten().any(|time| time > now) {
        Err(Error::InvalidResponse)
    } else {
        Ok(())
    }
}
fn validate_job_state(state: Option<&str>) -> Result<()> {
    if matches!(
        state,
        None | Some("queued" | "importing" | "pointer" | "done")
    ) {
        Ok(())
    } else {
        Err(Error::InvalidResponse)
    }
}
fn append_alerts(
    feed: &FeedStatus,
    active_age: Option<i64>,
    job_update_age: Option<i64>,
    alerts: &mut Vec<Alert>,
) {
    let stale = |age: Option<i64>| age.is_some_and(|n| n >= STALLED_SECONDS);
    let mut alert = |kind| {
        alerts.push(Alert {
            election_id: feed.election_id,
            kind,
        });
    };
    if feed.pending_job.is_some() && feed.job_attempts >= 8 {
        alert(AlertKind::QuarantinedJob);
    }
    if feed.error_present && feed.job_attempts >= 3 {
        alert(AlertKind::RepeatedErrors);
    }
    if feed.pending_job.is_some() && stale(feed.pending_age_seconds) {
        alert(AlertKind::PublicationBacklog);
    }
    if feed.pending_job.is_some() && stale(job_update_age) {
        alert(AlertKind::StalledJob);
    }
    if feed.enabled
        && feed.checks_expected
        && feed
            .source_check_age_seconds
            .or(active_age)
            .is_some_and(|n| n >= feed.source_check_stale_after_seconds)
    {
        alert(AlertKind::StaleSourceChecks);
    }
    if feed.enabled
        && feed.checks_expected
        && feed
            .successful_check_age_seconds
            .or(active_age)
            .is_some_and(|n| n >= feed.source_check_stale_after_seconds)
    {
        alert(AlertKind::StaleSuccessfulChecks);
    }
    if stale(feed.pointer_mismatch_age_seconds) {
        alert(AlertKind::PointerDivergence);
    }
    if matches!(feed.pointer, PointerObservation::Unavailable) {
        alert(AlertKind::PointerUnavailable);
    }
}
fn check_threshold(interval: u32) -> Result<i64> {
    if !(60..=86400).contains(&interval) {
        return Err(Error::InvalidResponse);
    }
    Ok(STALLED_SECONDS.max(i64::from(interval) * 2 + 300))
}
fn cadence_from_environment(now: DateTime<Utc>) -> Result<u32> {
    let value = |name| std::env::var(name).ok();
    effective_cadence(
        value("SOS_POLL_INITIAL_INTERVAL_SECONDS").as_deref(),
        value("SOS_POLL_TRANSITION_AT").as_deref(),
        value("SOS_POLL_INTERVAL_SECONDS").as_deref(),
        now,
    )
}
/// Transitional fallback for older operational RPCs; new schemas expose effective cadence.
/// # Errors
/// Rejects invalid intervals and transition timestamps; never changes the actual scheduler.
pub fn effective_cadence(
    initial: Option<&str>,
    transition: Option<&str>,
    steady: Option<&str>,
    now: DateTime<Utc>,
) -> Result<u32> {
    let parse = |text: Option<&str>, default: u32| {
        text.map_or(Ok(default), |text| {
            text.parse::<u32>()
                .map_err(|_| Error::Config("Invalid monitoring cadence"))
        })
    };
    let steady = parse(steady, 300)?;
    let initial = parse(initial, steady)?;
    check_threshold(initial)?;
    check_threshold(steady)?;
    let interval = if let Some(transition) = transition.filter(|value| !value.is_empty()) {
        let at: DateTime<Utc> = transition
            .parse()
            .map_err(|_| Error::Config("Invalid monitoring cadence transition"))?;
        if now < at { initial } else { steady }
    } else {
        steady
    };
    Ok(interval)
}
fn revision_zero() -> Revision {
    Revision::try_from("0".to_owned()).expect("static decimal revision")
}
fn age(time: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<i64> {
    time.map(|time| now.signed_duration_since(time).num_seconds().max(0))
}

/// Read service health and bounded pointer documents through uncached S3, never a CDN.
/// # Errors
/// Rejects invalid RPC data or a monitoring pass exceeding its 60-second deadline.
pub async fn collect(
    rpc: &Rpc,
    store: &dyn Store,
    now: DateTime<Utc>,
    state: &mut ObservationState,
) -> Result<Report> {
    let operation = async {
        let rows: Vec<HealthRow> = rpc.call("get_sos_collection_health", json!({})).await?;
        if rows.len() > 512 {
            return Err(Error::ResponseLimit);
        }
        let mut observed = Vec::with_capacity(rows.len());
        for mut row in rows {
            row.checks_expected = if let Some(expected) = row.source_checks_expected {
                expected
            } else {
                let operational: serde_json::Value = rpc
                    .call(
                        "get_sos_results_health",
                        json!({"p_election_id":row.election_id}),
                    )
                    .await?;
                match operational["status"].as_str() {
                    Some("paused" | "missing") => false,
                    Some("delayed" | "queued" | "healthy") => true,
                    _ => return Err(Error::InvalidResponse),
                }
            };
            if row.poll_interval_seconds.is_none() {
                row.poll_interval_seconds = Some(cadence_from_environment(now)?);
            }
            let key = format!("elections/{}/latest.json", row.election_id);
            let pointer =
                match tokio::time::timeout(Duration::from_secs(5), store.get(&key, MAX_POINTER))
                    .await
                {
                    Ok(Ok(object)) if object.status == 404 => PointerObservation::Missing,
                    Ok(Ok(object)) if object.status == 200 => {
                        (
                            row.source_to_publication_lag_seconds,
                            row.ingestion_lag_seconds,
                        ) = pointer_lags(&object.body);
                        parse_pointer(&object.body, row.election_id)
                    }
                    _ => PointerObservation::Unavailable,
                };
            observed.push((row, pointer));
        }
        evaluate(observed, now, state)
    };
    tokio::time::timeout(Duration::from_secs(60), operation)
        .await
        .map_err(|_| Error::Timeout)?
}
fn pointer_lags(body: &[u8]) -> (Option<i64>, Option<i64>) {
    #[derive(Deserialize)]
    struct Times {
        #[serde(rename = "source_created_at")]
        source: Option<DateTime<Utc>>,
        #[serde(rename = "retrieved_at")]
        retrieved: Option<DateTime<Utc>>,
        #[serde(rename = "published_at")]
        published: Option<DateTime<Utc>>,
    }
    let Ok(times) = serde_json::from_slice::<Times>(body) else {
        return (None, None);
    };
    let lag = |from: Option<DateTime<Utc>>| {
        from.zip(times.published)
            .map(|(from, to)| to.signed_duration_since(from).num_seconds())
            .filter(|seconds| *seconds >= 0)
    };
    (lag(times.source), lag(times.retrieved))
}
fn parse_pointer(bytes: &[u8], election: Uuid) -> PointerObservation {
    #[derive(Deserialize)]
    struct Identity {
        schema_version: u32,
        election_id: Uuid,
        snapshot_id: Uuid,
        revision: Revision,
    }
    match serde_json::from_slice::<Identity>(bytes) {
        Ok(value)
            if value.schema_version == 1
                && value.election_id == election
                && !value.snapshot_id.is_nil() =>
        {
            PointerObservation::Present {
                revision: value.revision,
            }
        }
        _ => PointerObservation::Unavailable,
    }
}
/// Render stable metrics. Exact revisions stay strings in JSON, never lossy float gauges.
#[must_use]
pub fn prometheus(report: &Report) -> String {
    use std::fmt::Write;
    let mut text = String::from(
        "# HELP civpulse_sos_monitor_success Last monitoring pass completed.\n# TYPE civpulse_sos_monitor_success gauge\n",
    );
    let success = u8::from(
        !report
            .alerts
            .iter()
            .any(|alert| alert.kind == AlertKind::MonitoringUnavailable),
    );
    let _ = writeln!(text, "civpulse_sos_monitor_success {success}");
    for feed in &report.feeds {
        let id = feed.election_id;
        let job = feed
            .pending_job
            .map_or_else(String::new, |id| id.to_string());
        let _ = writeln!(
            text,
            "civpulse_sos_job_pending{{election_id=\"{id}\",job_id=\"{job}\"}} {}",
            u8::from(feed.pending_job.is_some())
        );
        let matched = match &feed.pointer {
            PointerObservation::Missing => i8::from(feed.database_revision == revision_zero()),
            PointerObservation::Present { revision } => {
                i8::from(*revision == feed.database_revision)
            }
            PointerObservation::Unavailable => -1,
        };
        for (name, value) in [
            ("feed_enabled", i64::from(feed.enabled)),
            ("source_checks_expected", i64::from(feed.checks_expected)),
            (
                "poll_interval_seconds",
                i64::from(feed.poll_interval_seconds),
            ),
            (
                "source_check_stale_after_seconds",
                feed.source_check_stale_after_seconds,
            ),
            ("job_attempts", i64::from(feed.job_attempts)),
            (
                "source_to_publication_lag_seconds",
                feed.source_to_publication_lag_seconds.unwrap_or(-1),
            ),
            (
                "ingestion_lag_seconds",
                feed.ingestion_lag_seconds.unwrap_or(-1),
            ),
            (
                "job_progress_age_seconds",
                feed.job_progress_age_seconds.unwrap_or(-1),
            ),
            ("error_present", i64::from(feed.error_present)),
            (
                "actual_source_check_age_seconds",
                feed.actual_source_check_age_seconds.unwrap_or(-1),
            ),
            (
                "source_check_age_seconds",
                feed.source_check_age_seconds.unwrap_or(-1),
            ),
            (
                "successful_check_age_seconds",
                feed.successful_check_age_seconds.unwrap_or(-1),
            ),
            (
                "pending_age_seconds",
                feed.pending_age_seconds.unwrap_or(-1),
            ),
            (
                "pointer_mismatch_age_seconds",
                feed.pointer_mismatch_age_seconds.unwrap_or(-1),
            ),
            ("pointer_matches_database", i64::from(matched)),
        ] {
            let _ = writeln!(text, "civpulse_sos_{name}{{election_id=\"{id}\"}} {value}");
        }
    }
    for alert in &report.alerts {
        let _ = writeln!(
            text,
            "civpulse_sos_alert{{election_id=\"{}\",kind=\"{}\"}} 1",
            alert.election_id,
            alert.kind.label()
        );
    }
    text
}
/// Persist bounded, credential-free observation state atomically in writable scratch.
/// # Errors
/// Rejects oversized/invalid state and local I/O failures.
pub async fn save_state(path: &Path, state: &ObservationState) -> Result<()> {
    let bytes = serde_json::to_vec(state).map_err(|_| Error::InvalidResponse)?;
    if bytes.len() > MAX_STATE {
        return Err(Error::ResponseLimit);
    }
    let parent = path
        .parent()
        .ok_or(Error::Config("Monitor state requires a parent directory"))?;
    let temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| Error::LocalIo)?;
    tokio::fs::write(temp.path(), bytes)
        .await
        .map_err(|_| Error::LocalIo)?;
    temp.persist(path).map_err(|_| Error::LocalIo)?;
    Ok(())
}
fn observation_lock(path: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path.with_extension("lock"))
        .map_err(|_| Error::LocalIo)?;
    file.try_lock_exclusive()
        .map_err(|_| Error::Source("Another monitor owns the observation state lock"))?;
    Ok(file)
}
/// Missing state starts a new observation window; corruption fails closed.
/// # Errors
/// Rejects oversized/invalid state and local I/O failures.
pub async fn load_state(path: &Path) -> Result<ObservationState> {
    match tokio::fs::metadata(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ObservationState::default())
        }
        Ok(metadata) if metadata.len() <= MAX_STATE as u64 => {
            serde_json::from_slice(&tokio::fs::read(path).await.map_err(|_| Error::LocalIo)?)
                .map_err(|_| Error::InvalidResponse)
        }
        Ok(_) => Err(Error::ResponseLimit),
        Err(_) => Err(Error::LocalIo),
    }
}
#[derive(Clone, Copy)]
pub enum Format {
    Json,
    Prometheus,
}
/// One-shot remote read-only command; an alert produces sanitized output and nonzero status.
/// # Errors
/// Rejects invalid environment, unhealthy dependencies, invalid state or actionable alerts.
pub async fn command(rpc: &Rpc, format: Format) -> Result<()> {
    let report = observe(rpc).await?;
    render(&report, format)?;
    if report.alerts.is_empty() {
        Ok(())
    } else {
        Err(Error::Source(
            "Operational monitoring found actionable alerts",
        ))
    }
}
/// Collect and persist one sanitized read-only operational observation.
/// # Errors
/// Rejects missing configuration, invalid local state or operational transport failure.
pub async fn observe(rpc: &Rpc) -> Result<Report> {
    let required = |key| {
        std::env::var(key)
            .map_err(|_| Error::Config("Monitoring requires environment-scoped R2 credentials"))
    };
    let store = R2Store::new(
        &required("CIVPULSE_ENV")?,
        &required("R2_ENDPOINT")?,
        &required("R2_BUCKET")?,
        &required("R2_PUBLIC_ORIGIN")?,
        &required("R2_ACCESS_KEY_ID")?,
        &required("R2_SECRET_ACCESS_KEY")?,
    )?;
    let path = std::env::var("SOS_MONITOR_STATE").map_or_else(
        |_| std::path::PathBuf::from("/tmp/civpulse-results-monitor-state.json"),
        std::path::PathBuf::from,
    );
    let _lock = observation_lock(&path)?;
    let mut state = load_state(&path).await?;
    let report = collect(rpc, &store, Utc::now(), &mut state).await?;
    save_state(&path, &state).await?;
    Ok(report)
}
/// Render only sanitized operational fields.
/// # Errors
/// Returns an error when JSON serialization fails.
pub fn render(report: &Report, format: Format) -> Result<()> {
    match format {
        Format::Json => println!(
            "{}",
            serde_json::to_string(&report).map_err(|_| Error::InvalidResponse)?
        ),
        Format::Prometheus => print!("{}", prometheus(report)),
    }
    Ok(())
}
/// A missing monitor observation is actionable independently of vote changes.
#[must_use]
pub fn unavailable() -> Report {
    Report {
        checked_at: Utc::now(),
        feeds: vec![],
        alerts: vec![Alert {
            election_id: Uuid::nil(),
            kind: AlertKind::MonitoringUnavailable,
        }],
    }
}

#[cfg(test)]
mod lag_tests {
    use super::*;
    #[test]
    fn unsupported_times_are_unknown_and_old_official_time_is_informational() {
        assert_eq!(pointer_lags(b"{}"), (None, None));
        assert_eq!(pointer_lags(br#"{"source_created_at":"2026-11-03T10:00:00Z","retrieved_at":"2026-11-03T17:59:55Z","published_at":"2026-11-03T18:00:00Z"}"#),(Some(28800),Some(5)));
        assert_eq!(pointer_lags(br#"{"source_created_at":"2026-11-03T19:00:00Z","retrieved_at":null,"published_at":"2026-11-03T18:00:00Z"}"#),(None,None));
    }
}
