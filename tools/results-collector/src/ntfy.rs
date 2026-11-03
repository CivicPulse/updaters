//! Public-topic notifications contain only the reviewed public election and safe condition labels.
use crate::{
    Error, Result,
    alerting::{self, AlertState, Delivery, Mail, Outcome, Receipt},
    monitoring::{Alert, PointerObservation, Report},
};
use async_trait::async_trait;
use civpulse_results_contract::Revision;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

pub const TOPIC: &str = "https://ntfy.sh/CivPulse";
pub const PUBLIC_ELECTION: Uuid = Uuid::from_u128(0x295d_864d_80c8_4a87_ab16_3f4f_3fa5_ddb7);
const MAX: usize = 8192;
#[derive(Deserialize)]
struct Ack {
    id: String,
    topic: String,
    event: String,
}
const DEADLINE: Duration = Duration::from_secs(15);

pub struct Ntfy {
    client: Client,
}
impl Ntfy {
    /// Fixed public destination; no credentials, redirects, source URLs or arbitrary endpoints.
    /// # Errors
    /// Returns a sanitized client configuration failure.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(DEADLINE)
                .build()
                .map_err(|_| Error::Source("Cannot initialize push transport"))?,
        })
    }
    async fn request(&self, endpoint: &str, subject: &str, text: &str) -> Result<Receipt> {
        if subject.is_empty()
            || subject.len() > 200
            || subject.contains(['\r', '\n'])
            || text.len() > 4096
        {
            return Err(Error::Config("Push message exceeds safe bounds"));
        }
        let operation = async {
            let mut response = self
                .client
                .post(endpoint)
                .header("Title", subject)
                .header("Content-Type", "text/plain; charset=utf-8")
                .body(text.to_owned())
                .send()
                .await
                .map_err(|_| Error::Source("Push transport request failed"))?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(Error::Source("Push transport rejected the request"));
            }
            if response.content_length().is_some_and(|n| n > MAX as u64) {
                return Err(Error::ResponseLimit);
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Error::Source("Push response read failed"))?
            {
                if chunk.len() > MAX.saturating_sub(body.len()) {
                    return Err(Error::ResponseLimit);
                }
                body.extend_from_slice(&chunk);
            }
            let ack: Ack = serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse)?;
            if ack.topic != "CivPulse"
                || ack.event != "message"
                || ack.id.is_empty()
                || ack.id.len() > 128
                || !ack.id.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(Error::InvalidResponse);
            }
            Ok(Receipt {
                delivery: Delivery::Delivered,
                message_id: ack.id,
            })
        };
        tokio::time::timeout(DEADLINE, operation)
            .await
            .map_err(|_| Error::Timeout)?
    }
}
#[async_trait]
impl Mail for Ntfy {
    async fn send(&self, subject: &str, text: &str) -> Result<Receipt> {
        self.request(TOPIC, subject, text).await
    }
}
#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub environment: Option<String>,
    pub alerts: AlertState,
    pub publication: Option<Publication>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Publication {
    pub revision: Revision,
    pub fingerprint: String,
}

fn confirmed(report: &Report) -> Option<Publication> {
    let feed = report
        .feeds
        .iter()
        .find(|feed| feed.election_id == PUBLIC_ELECTION)?;
    if feed.pending_job.is_some() {
        return None;
    }
    let PointerObservation::Present { revision } = &feed.pointer else {
        return None;
    };
    if revision != &feed.database_revision {
        return None;
    }
    let fingerprint = feed.publication_fingerprint.as_ref()?;
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some(Publication {
        revision: revision.clone(),
        fingerprint: fingerprint.clone(),
    })
}
/// Whether an alert may be sent to the fixed public topic.
#[must_use]
pub fn public_alert(alert: &Alert) -> bool {
    alert.election_id == PUBLIC_ELECTION || alert.election_id.is_nil()
}

/// Deliver independent actionable conditions and semantic publication advances.
/// First confirmed observation silently establishes a baseline. A failed send retries next run;
/// crashes after provider acceptance may duplicate a notification (at-least-once delivery).
/// # Errors
/// Returns sanitized transport/configuration errors; successful peer state remains advanced.
pub async fn deliver(
    report: &Report,
    environment: &str,
    transport: &dyn Mail,
    state: &mut State,
) -> Result<Outcome> {
    if environment != "prod"
        || state
            .alerts
            .environment
            .as_deref()
            .is_some_and(|env| env != "prod")
        || state
            .environment
            .as_deref()
            .is_some_and(|env| env != "prod")
    {
        return Err(Error::Config(
            "Public push notifications require production",
        ));
    }
    state.environment = Some("prod".into());
    let public_report = Report {
        checked_at: report.checked_at,
        feeds: vec![],
        alerts: report
            .alerts
            .iter()
            .filter(|alert| public_alert(alert))
            .map(|alert| Alert {
                election_id: alert.election_id,
                kind: alert.kind,
            })
            .collect(),
    };
    let errors = alerting::deliver(&public_report, environment, transport, &mut state.alerts).await;
    let mut data = Ok(Outcome::Quiet);
    if let Some(current) = confirmed(report) {
        match state.publication.as_ref() {
            None => state.publication = Some(current),
            Some(prior) if current.revision > prior.revision => {
                if current.fingerprint == prior.fingerprint {
                    state.publication = Some(current);
                } else {
                    let text = format!(
                        "New Georgia November 3, 2026 election results are published (revision {}).\nhttps://www.civpulse.org/elections/2026-11-03-general-election/\nSource reports may be preliminary; this is not certification.",
                        String::from(current.revision.clone())
                    );
                    data = transport
                        .send("CivicPulse prod: new election results", &text)
                        .await
                        .map(|_| {
                            state.publication = Some(current);
                            Outcome::Delivered
                        });
                }
            }
            _ => {}
        }
    }
    let errors = errors?;
    let data = data?;
    Ok(if data == Outcome::Delivered {
        data
    } else {
        errors
    })
}
/// Persistent independent state on the monitoring PVC. No source/DB writes occur.
/// # Errors
/// Rejects invalid opt-in/environment/endpoint; retains failed delivery for a later run.
pub async fn command(report: &Report) -> Result<Outcome> {
    match std::env::var("SOS_NTFY_ENABLED").as_deref() {
        Err(_) | Ok("false") => return Ok(Outcome::Disabled),
        Ok("true") => {}
        _ => return Err(Error::Config("SOS_NTFY_ENABLED must be true or false")),
    }
    if std::env::var("CIVPULSE_ENV").as_deref() != Ok("prod")
        || std::env::var("SOS_NTFY_URL").as_deref() != Ok(TOPIC)
    {
        return Err(Error::Config("Push requires the approved production topic"));
    }
    let path = std::env::var("SOS_NTFY_STATE").map_or_else(
        |_| PathBuf::from("/tmp/civpulse-results-ntfy.json"),
        PathBuf::from,
    );
    let _lock = alerting::lock(&path)?;
    let mut state: State = alerting::load(&path).await?;
    let result = deliver(report, "prod", &Ntfy::new()?, &mut state).await;
    // Persist successful error/new-data state even if the other notification failed.
    alerting::save(&path, &state).await?;
    tracing::info!(event = "push_delivery", success = result.is_ok());
    result
}

/// Explicit live route verification, with no source/DB credentials or deduplication mutations.
/// # Errors
/// Requires production opt-in, the approved topic, and disabled collection.
pub async fn verification() -> Result<Receipt> {
    if std::env::var("SOS_NTFY_ENABLED").as_deref() != Ok("true") {
        return Err(Error::Disabled);
    }
    if std::env::var("CIVPULSE_ENV").as_deref() != Ok("prod")
        || std::env::var("SOS_NTFY_URL").as_deref() != Ok(TOPIC)
        || std::env::var("SOS_COLLECTOR_ENABLED").as_deref() != Ok("false")
    {
        return Err(Error::Config(
            "Push verification requires approved production topic and disabled collection",
        ));
    }
    Ntfy::new()?.send("CivicPulse TEST: notification route verification", "TEST ONLY: requested CivicPulse results notification route verification. This is neither a collection error nor a new publication. No source check, job, publication, or feed configuration was changed.").await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn transport_requires_bounded_matching_ack_and_rejects_redirects_without_leaks() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for (status, extra, body, accepted) in [
            (
                "200 OK",
                "",
                r#"{"id":"abc123","topic":"CivPulse","event":"message"}"#,
                true,
            ),
            (
                "200 OK",
                "",
                r#"{"id":"abc123","topic":"Other","event":"message"}"#,
                false,
            ),
            (
                "200 OK",
                "",
                r#"{"id":"abc123","topic":"CivPulse","event":"keepalive"}"#,
                false,
            ),
            (
                "302 Found",
                "Location: https://private.example/secret\r\n",
                "",
                false,
            ),
            ("429 Too Many Requests", "", "private-token", false),
            ("200 OK", "Content-Length: 1000000\r\n", "", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 2048];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            assert!(headers.starts_with("post / http/1.1"));
                            assert!(!headers.contains("authorization"));
                            assert!(headers.contains("title: civicpulse test"));
                            break;
                        }
                    }
                }
                let length = if extra.contains("Content-Length") {
                    String::new()
                } else {
                    format!("Content-Length: {}\r\n", body.len())
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\n{extra}{length}Connection: close\r\n\r\n{body}"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let result = Ntfy::new()
                .unwrap()
                .request(&url, "CivicPulse TEST", "Safe notification")
                .await;
            assert_eq!(result.is_ok(), accepted);
            if let Err(error) = result {
                assert!(!error.to_string().contains("private"));
            }
            server.await.unwrap();
        }
    }
}
