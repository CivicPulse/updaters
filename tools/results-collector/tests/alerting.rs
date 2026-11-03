use async_trait::async_trait;
use chrono::Utc;
use civpulse_results_collector::{
    Error, Result,
    alerting::{self, AlertState, Delivery, Mail, Outcome, Receipt},
    monitoring::{Alert, AlertKind, Report},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use uuid::Uuid;
fn report(kinds: &[AlertKind]) -> Report {
    Report {
        checked_at: Utc::now(),
        feeds: vec![],
        alerts: kinds
            .iter()
            .map(|kind| Alert {
                election_id: Uuid::nil(),
                kind: *kind,
            })
            .collect(),
    }
}
#[derive(Default)]
struct Sender {
    calls: AtomicUsize,
    fail: AtomicBool,
}
#[async_trait]
impl Mail for Sender {
    async fn send(&self, subject: &str, text: &str) -> Result<Receipt> {
        assert!(subject.starts_with("CivicPulse test:"));
        assert!(!text.contains("private-token"));
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            Err(Error::Source("Simulated provider failure"))
        } else {
            Ok(Receipt {
                delivery: Delivery::Delivered,
                message_id: "test-message-id".into(),
            })
        }
    }
}
#[tokio::test]
async fn changed_conditions_and_recovery_send_once_and_unchanged_stays_quiet() {
    let sender = Sender::default();
    let mut state = AlertState::default();
    let healthy = report(&[]);
    assert_eq!(
        alerting::deliver(&healthy, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Quiet
    );
    assert_eq!(sender.calls.load(Ordering::Relaxed), 0);
    let initial = report(&[AlertKind::StalledJob]);
    assert_eq!(
        alerting::deliver(&initial, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Delivered
    );
    assert_eq!(
        alerting::deliver(&initial, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Quiet
    );
    let escalated = report(&[AlertKind::StalledJob, AlertKind::QuarantinedJob]);
    alerting::deliver(&escalated, "test", &sender, &mut state)
        .await
        .unwrap();
    let reordered = report(&[AlertKind::QuarantinedJob, AlertKind::StalledJob]);
    assert_eq!(
        alerting::deliver(&reordered, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Quiet
    );
    assert_eq!(
        alerting::deliver(&healthy, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Delivered
    );
    assert_eq!(
        alerting::deliver(&healthy, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Quiet
    );
    assert_eq!(sender.calls.load(Ordering::Relaxed), 3);
    assert!(!state.had_alerts);
    assert_eq!(state.last_message_id.as_deref(), Some("test-message-id"));
}
#[tokio::test]
async fn failed_delivery_never_advances_deduplication_state() {
    let sender = Sender::default();
    sender.fail.store(true, Ordering::Relaxed);
    let mut state = AlertState::default();
    let failed = report(&[AlertKind::MonitoringUnavailable]);
    assert!(
        alerting::deliver(&failed, "test", &sender, &mut state)
            .await
            .is_err()
    );
    assert!(state.fingerprint.is_none());
    assert!(!state.had_alerts);
    sender.fail.store(false, Ordering::Relaxed);
    assert_eq!(
        alerting::deliver(&failed, "test", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Delivered
    );
}
#[test]
fn state_cannot_be_shared_between_environments_and_payload_is_sanitized() {
    let state = AlertState {
        environment: Some("prod".into()),
        ..AlertState::default()
    };
    let report = report(&[AlertKind::PointerDivergence]);
    assert!(alerting::plan(&report, "test", &state).is_err());
    let plan = alerting::plan(&report, "prod", &AlertState::default())
        .unwrap()
        .unwrap();
    assert!(plan.text.contains("Kerry Hatcher"));
    assert!(!plan.text.contains("raw"));
    assert!(plan.subject.contains("prod"));
}
#[test]
fn verification_requires_opt_in_and_never_needs_source_credentials() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_civpulse-results-collector"))
        .arg("alert-test")
        .env_remove("SOS_ALERTS_ENABLED")
        .env_remove("CLOUDFLARE_EMAIL_API_TOKEN")
        .env_remove("SUPABASE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Collection disabled"));
    assert_eq!(output.stdout.len(), 0);
}

#[test]
fn resources_command_reads_only_owned_kernel_counters_without_credentials() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_civpulse-results-collector"))
        .arg("resources")
        .env_remove("SUPABASE_URL")
        .env_remove("SUPABASE_SERVICE_ROLE_KEY")
        .env_remove("R2_SECRET_ACCESS_KEY")
        .env_remove("CLOUDFLARE_EMAIL_API_TOKEN")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.get("cgroup_memory_current_bytes").is_some());
    assert!(value.get("process_rss_bytes").is_some());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("token"));
    let rejected = std::process::Command::new(env!("CARGO_BIN_EXE_civpulse-results-collector"))
        .args(["resources", "/etc/passwd"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
}
