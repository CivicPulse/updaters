use async_trait::async_trait;
use chrono::Utc;
use civpulse_results_collector::{
    Error, Result,
    alerting::{self, Delivery, Mail, Outcome, Receipt},
    monitoring::{self, Alert, AlertKind, HealthRow, ObservationState, PointerObservation, Report},
    ntfy::{self, PUBLIC_ELECTION, State},
};
use civpulse_results_contract::Revision;
use serde_json::json;
use std::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
struct Sender {
    messages: Mutex<Vec<(String, String)>>,
    fail_subject: Mutex<Option<String>>,
}
#[async_trait]
impl Mail for Sender {
    async fn send(&self, subject: &str, text: &str) -> Result<Receipt> {
        self.messages
            .lock()
            .unwrap()
            .push((subject.into(), text.into()));
        if self
            .fail_subject
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|part| subject.contains(part))
        {
            return Err(Error::Source("Simulated rejection"));
        }
        Ok(Receipt {
            delivery: Delivery::Delivered,
            message_id: "message1".into(),
        })
    }
}
fn report(rev: &str, hash: Option<&str>) -> Report {
    let now = Utc::now();
    let mut row:HealthRow=serde_json::from_value(json!({"election_id":PUBLIC_ELECTION,"enabled":true,"last_attempt_at":now,"last_success_at":now,"last_error":null,"current_revision":rev,"job_id":null,"job_state":null,"job_attempts":null,"job_updated_at":null})).unwrap();
    row.publication_fingerprint = hash.map(str::to_owned);
    monitoring::evaluate(
        vec![(
            row,
            PointerObservation::Present {
                revision: Revision::try_from(rev.to_owned()).unwrap(),
            },
        )],
        now,
        &mut ObservationState::default(),
    )
    .unwrap()
}
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
#[tokio::test]
async fn bootstrap_noop_formatting_and_regressions_are_quiet_semantic_advances_send_once() {
    let sender = Sender::default();
    let mut state = State::default();
    for (rev, hash) in [("1", A), ("1", A), ("2", A), ("1", B)] {
        assert_eq!(
            ntfy::deliver(&report(rev, Some(hash)), "prod", &sender, &mut state)
                .await
                .unwrap(),
            Outcome::Quiet
        );
    }
    assert!(sender.messages.lock().unwrap().is_empty());
    let changed = report("9007199254740993", Some(B));
    assert_eq!(
        ntfy::deliver(&changed, "prod", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Delivered
    );
    assert_eq!(
        ntfy::deliver(&changed, "prod", &sender, &mut state)
            .await
            .unwrap(),
        Outcome::Quiet
    );
    assert_eq!(sender.messages.lock().unwrap().len(), 1);
    assert!(
        sender.messages.lock().unwrap()[0]
            .1
            .contains("9007199254740993")
    );
}
#[tokio::test]
async fn missing_malformed_unconfirmed_pending_and_private_observations_never_seed_or_send() {
    let sender = Sender::default();
    let mut state = State::default();
    let mut cases = vec![report("1", None), report("1", Some("secret"))];
    let mut mismatch = report("1", Some(A));
    mismatch.feeds[0].database_revision = Revision::try_from("2".to_owned()).unwrap();
    cases.push(mismatch);
    let mut pending = report("1", Some(A));
    pending.feeds[0].pending_job = Some(Uuid::from_u128(1));
    cases.push(pending);
    let mut private = report("1", Some(A));
    private.feeds[0].election_id = Uuid::from_u128(1);
    private.alerts.push(Alert {
        election_id: private.feeds[0].election_id,
        kind: AlertKind::RepeatedErrors,
    });
    cases.push(private);
    for report in cases {
        ntfy::deliver(&report, "prod", &sender, &mut state)
            .await
            .unwrap();
        assert!(state.publication.is_none());
    }
    assert!(sender.messages.lock().unwrap().is_empty());
    assert!(
        ntfy::deliver(&report("1", Some(A)), "test", &sender, &mut state)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn initial_errors_send_but_data_baseline_is_quiet_and_successes_survive_peer_failure() {
    for failing in ["attention", "new election"] {
        let sender = Sender::default();
        let mut state = State::default();
        ntfy::deliver(&report("1", Some(A)), "prod", &sender, &mut state)
            .await
            .unwrap();
        *sender.fail_subject.lock().unwrap() = Some(failing.into());
        let mut changed = report("2", Some(B));
        changed.alerts.push(Alert {
            election_id: PUBLIC_ELECTION,
            kind: AlertKind::RepeatedErrors,
        });
        assert!(
            ntfy::deliver(&changed, "prod", &sender, &mut state)
                .await
                .is_err()
        );
        assert_eq!(sender.messages.lock().unwrap().len(), 2);
        assert_eq!(state.alerts.had_alerts, failing != "attention");
        assert_eq!(
            String::from(state.publication.as_ref().unwrap().revision.clone()),
            if failing == "attention" { "2" } else { "1" }
        );
        let encoded = serde_json::to_vec(&state).unwrap();
        let mut restarted: State = serde_json::from_slice(&encoded).unwrap();
        *sender.fail_subject.lock().unwrap() = None;
        ntfy::deliver(&changed, "prod", &sender, &mut restarted)
            .await
            .unwrap();
        assert_eq!(sender.messages.lock().unwrap().len(), 3);
        assert_eq!(
            ntfy::deliver(&changed, "prod", &sender, &mut restarted)
                .await
                .unwrap(),
            Outcome::Quiet
        );
    }
    let sender = Sender::default();
    let mut state = State::default();
    let mut first = report("1", Some(A));
    first.alerts.push(Alert {
        election_id: Uuid::nil(),
        kind: AlertKind::MonitoringUnavailable,
    });
    ntfy::deliver(&first, "prod", &sender, &mut state)
        .await
        .unwrap();
    assert_eq!(sender.messages.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn email_and_push_delivery_state_remain_independent_on_channel_failure() {
    let email = Sender::default();
    let push = Sender::default();
    *email.fail_subject.lock().unwrap() = Some("attention".into());
    let mut email_state = alerting::AlertState::default();
    let mut push_state = State::default();
    let mut observed = report("1", Some(A));
    observed.alerts.push(Alert {
        election_id: PUBLIC_ELECTION,
        kind: AlertKind::StalledJob,
    });
    let (mail, ntfy) = tokio::join!(
        alerting::deliver(&observed, "prod", &email, &mut email_state),
        ntfy::deliver(&observed, "prod", &push, &mut push_state)
    );
    assert!(mail.is_err());
    assert!(ntfy.is_ok());
    assert!(email_state.fingerprint.is_none());
    assert!(push_state.alerts.had_alerts);
}
#[test]
fn route_verification_defaults_disabled_without_source_credentials() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_civpulse-results-collector"))
        .arg("ntfy-test")
        .env_remove("SOS_NTFY_ENABLED")
        .env_remove("SUPABASE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout.len(), 0);
}

#[tokio::test]
async fn mixed_alerts_send_only_public_eligible_conditions() {
    let sender = Sender::default();
    let private = Uuid::from_u128(1);
    let report = Report {
        checked_at: Utc::now(),
        feeds: vec![],
        alerts: [private, PUBLIC_ELECTION, Uuid::nil()]
            .into_iter()
            .map(|election_id| Alert {
                election_id,
                kind: AlertKind::RepeatedErrors,
            })
            .collect(),
    };
    ntfy::deliver(&report, "prod", &sender, &mut State::default())
        .await
        .unwrap();
    let messages = sender.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].1.contains(&PUBLIC_ELECTION.to_string()));
    assert!(messages[0].1.contains("deployment monitor:"));
    assert!(!messages[0].1.contains(&private.to_string()));
}
