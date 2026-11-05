//! Credential-scoped transactional alerts; unchanged alert sets stay quiet.
use crate::{Error, Result, monitoring::Report, storage::sha256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

pub const ACCOUNT: &str = "6d837580a4d0641139ecada9e74076b8";
pub const FROM: &str = "results@alerts.civpulse.org";
pub const RECIPIENT: &str = "kerry@kerryhatcher.com";
const MAX_RESPONSE: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Delivered,
    Queued,
}
#[async_trait]
pub trait Mail: Send + Sync {
    async fn send(&self, subject: &str, text: &str) -> Result<Receipt>;
}
#[derive(Debug, Serialize)]
pub struct Receipt {
    pub delivery: Delivery,
    pub message_id: String,
}
pub struct CloudflareMail {
    client: Client,
    headers: HeaderMap,
}
impl CloudflareMail {
    /// Create a narrowly scoped Cloudflare Email Sending client for the `CivicPulse` account.
    /// # Errors
    /// Rejects missing/invalid tokens or client setup failures.
    pub fn new(token: &str) -> Result<Self> {
        if token.is_empty() {
            return Err(Error::Config("Email Sending token is required"));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::Config("Invalid Email Sending token"))?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| Error::Source("Cannot initialize alert transport"))?;
        Ok(Self { client, headers })
    }
    async fn request(&self, url: &str, subject: &str, text: &str) -> Result<Receipt> {
        if subject.is_empty()
            || subject.len() > 200
            || subject.contains(['\n', '\r'])
            || text.len() > 32 * 1024
        {
            return Err(Error::Config("Alert message exceeds safe bounds"));
        }
        let operation = async {
            let mut response=self.client.post(url).headers(self.headers.clone()).json(&json!({"from":{"address":FROM,"name":"CivicPulse results"},"to":RECIPIENT,"subject":subject,"text":text,"html":format!("<pre>{}</pre>",text.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;"))})).send().await.map_err(|_|Error::Source("Alert transport request failed"))?;
            if response.status().as_u16() != 200 {
                tracing::error!(
                    event = "alert_transport_http_error",
                    http_status = response.status().as_u16()
                );
                return Err(Error::Source("Alert transport rejected the request"));
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE as u64)
            {
                return Err(Error::ResponseLimit);
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Error::Source("Alert transport read failed"))?
            {
                if chunk.len() > MAX_RESPONSE.saturating_sub(body.len()) {
                    return Err(Error::ResponseLimit);
                }
                body.extend_from_slice(&chunk);
            }
            parse_response(&body)
        };
        tokio::time::timeout(Duration::from_secs(30), operation)
            .await
            .map_err(|_| Error::Timeout)?
    }
}
#[async_trait]
impl Mail for CloudflareMail {
    async fn send(&self, subject: &str, text: &str) -> Result<Receipt> {
        self.request(
            &format!("https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/email/sending/send"),
            subject,
            text,
        )
        .await
    }
}
fn parse_response(body: &[u8]) -> Result<Receipt> {
    #[derive(Deserialize)]
    struct Response {
        success: bool,
        result: Option<Recipients>,
    }
    #[derive(Deserialize)]
    struct Recipients {
        message_id: String,
        delivered: Vec<String>,
        queued: Vec<String>,
        permanent_bounces: Vec<String>,
    }
    let response: Response = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    let result = response
        .result
        .filter(|_| response.success)
        .ok_or(Error::Source("Email Sending failed"))?;
    if result.message_id.is_empty()
        || result.message_id.len() > 512
        || !result.message_id.is_ascii()
        || result.message_id.contains(['\n', '\r'])
    {
        return Err(Error::InvalidResponse);
    }
    if !result.permanent_bounces.is_empty() {
        return Err(Error::Source("Alert recipient permanently bounced"));
    }
    if result.delivered.iter().any(|address| address == RECIPIENT) {
        Ok(Receipt {
            delivery: Delivery::Delivered,
            message_id: result.message_id,
        })
    } else if result.queued.iter().any(|address| address == RECIPIENT) {
        Ok(Receipt {
            delivery: Delivery::Queued,
            message_id: result.message_id,
        })
    } else {
        Err(Error::InvalidResponse)
    }
}
#[derive(Default, Serialize, Deserialize)]
pub struct AlertState {
    pub environment: Option<String>,
    pub fingerprint: Option<String>,
    pub had_alerts: bool,
    pub last_sent_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_message_id: Option<String>,
}
pub struct Notification {
    pub fingerprint: String,
    pub subject: String,
    pub text: String,
    pub recovery: bool,
}
/// Build only a changed actionable alert or a recovery; vote-count changes are irrelevant.
/// # Errors
/// Rejects an invalid environment or a deduplication state from another environment.
pub fn plan(
    report: &Report,
    environment: &str,
    state: &AlertState,
) -> Result<Option<Notification>> {
    if !matches!(environment, "dev" | "test" | "prod")
        || state
            .environment
            .as_deref()
            .is_some_and(|prior| prior != environment)
    {
        return Err(Error::Config("Alert state environment mismatch"));
    }
    let mut conditions: Vec<_> = report
        .alerts
        .iter()
        .map(|alert| (alert.election_id.to_string(), alert.kind.label()))
        .collect();
    conditions.sort_unstable();
    conditions.dedup();
    let fingerprint = sha256(
        &serde_json::to_vec(&(environment, &conditions)).map_err(|_| Error::InvalidResponse)?,
    );
    if state.fingerprint.as_deref() == Some(&fingerprint)
        || conditions.is_empty() && !state.had_alerts
    {
        return Ok(None);
    }
    let recovery = conditions.is_empty();
    let subject = format!(
        "CivicPulse {environment}: results {}",
        if recovery {
            "monitoring recovered"
        } else {
            "collection needs attention"
        }
    );
    let mut text = format!(
        "Operator: Kerry Hatcher\nEnvironment: {environment}\nObserved: {}\n\n",
        report.checked_at.to_rfc3339()
    );
    if recovery {
        text.push_str("All previously reported actionable conditions have cleared.\n");
    } else {
        text.push_str("Actionable conditions:\n");
        for (election, kind) in conditions.iter().take(100) {
            use std::fmt::Write;
            if election == &uuid::Uuid::nil().to_string() {
                let _ = writeln!(text, "- deployment monitor: {kind}");
            } else {
                let _ = writeln!(text, "- election {election}: {kind}");
            }
        }
        if conditions.len() > 100 {
            use std::fmt::Write;
            let _ = writeln!(
                text,
                "- {} additional conditions omitted; inspect monitor JSON",
                conditions.len() - 100
            );
        }
        text.push_str("\nInspect the read-only monitor and recovery runbook. Do not bypass validation, lease fencing, or conditional pointer publication.\n");
    }
    text.push_str("\nUnchanged source counts are healthy. This notification does not enable collection, alter a feed, or request a website rebuild.\n");
    Ok(Some(Notification {
        fingerprint,
        subject,
        text,
        recovery,
    }))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Disabled,
    Quiet,
    Delivered,
    Queued,
}
/// Send and persist only after acceptance by the transport. A process crash may duplicate a send.
/// # Errors
/// Returns delivery/persistence failures without advancing deduplication state.
pub async fn deliver(
    report: &Report,
    environment: &str,
    mail: &dyn Mail,
    state: &mut AlertState,
) -> Result<Outcome> {
    let Some(notification) = plan(report, environment, state)? else {
        return Ok(Outcome::Quiet);
    };
    let delivery = mail.send(&notification.subject, &notification.text).await?;
    state.environment = Some(environment.into());
    state.fingerprint = Some(notification.fingerprint);
    state.had_alerts = !notification.recovery;
    state.last_sent_at = Some(report.checked_at);
    state.last_message_id = Some(delivery.message_id);
    Ok(match delivery.delivery {
        Delivery::Delivered => Outcome::Delivered,
        Delivery::Queued => Outcome::Queued,
    })
}
pub(crate) fn lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
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
        .map_err(|_| Error::Source("Another alert notifier owns the state lock"))?;
    Ok(file)
}
pub(crate) async fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match tokio::fs::metadata(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Ok(metadata) if metadata.len() <= 16 * 1024 => {
            serde_json::from_slice(&tokio::fs::read(path).await.map_err(|_| Error::LocalIo)?)
                .map_err(|_| Error::InvalidResponse)
        }
        Ok(_) => Err(Error::ResponseLimit),
        Err(_) => Err(Error::LocalIo),
    }
}
pub(crate) async fn save<T: Serialize>(path: &Path, state: &T) -> Result<()> {
    let temp = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or(Error::Config("Alert state requires parent directory"))?,
    )
    .map_err(|_| Error::LocalIo)?;
    tokio::fs::write(
        temp.path(),
        serde_json::to_vec(state).map_err(|_| Error::InvalidResponse)?,
    )
    .await
    .map_err(|_| Error::LocalIo)?;
    temp.persist(path).map_err(|_| Error::LocalIo)?;
    Ok(())
}
/// Explicit notification command. Disabled by default; credentials stay in Secret-backed env.
/// # Errors
/// Returns sanitized configuration, local-state, or delivery failures.
pub async fn command(report: &Report) -> Result<Outcome> {
    match std::env::var("SOS_ALERTS_ENABLED").as_deref() {
        Err(_) | Ok("false") => return Ok(Outcome::Disabled),
        Ok("true") => {}
        _ => return Err(Error::Config("SOS_ALERTS_ENABLED must be true or false")),
    }
    let environment = std::env::var("CIVPULSE_ENV")
        .map_err(|_| Error::Config("Alert environment is required"))?;
    let token = std::env::var("CLOUDFLARE_EMAIL_API_TOKEN")
        .map_err(|_| Error::Config("Email Sending token is required"))?;
    let path = std::env::var("SOS_ALERT_STATE").map_or_else(
        |_| PathBuf::from("/tmp/civpulse-results-alert-state.json"),
        PathBuf::from,
    );
    let _lock = lock(&path)?;
    let mut state: AlertState = load(&path).await?;
    let mail = CloudflareMail::new(&token)?;
    let outcome = deliver(report, &environment, &mail, &mut state).await?;
    save(&path, &state).await?;
    tracing::info!(event = "alert_delivery", ?outcome, environment);
    Ok(outcome)
}
/// Explicit live alert-route verification; never claims or activates a source job.
/// # Errors
/// Requires opt-in alerts, environment and Secret-backed sending credentials.
pub async fn verification() -> Result<Receipt> {
    if std::env::var("SOS_ALERTS_ENABLED").as_deref() != Ok("true") {
        return Err(Error::Disabled);
    }
    let environment = std::env::var("CIVPULSE_ENV")
        .map_err(|_| Error::Config("Alert environment is required"))?;
    if !matches!(environment.as_str(), "dev" | "test" | "prod") {
        return Err(Error::Config("Invalid alert environment"));
    }
    let token = std::env::var("CLOUDFLARE_EMAIL_API_TOKEN")
        .map_err(|_| Error::Config("Email Sending token is required"))?;
    CloudflareMail::new(&token)?.send(&format!("CivicPulse {environment}: alert route verification"),"Requested verification of the deployed CivicPulse results alert sender.\n\nOperator: Kerry Hatcher. Recipient: kerry@kerryhatcher.com.\n\nThis verifies email delivery only. The verification job keeps collection disabled and cannot activate or change production collection.\n\nAfter acceptance and actual activation: immediate check; hourly until 2026-11-03T22:00:00Z, then every five minutes; stop at 2026-11-11T00:00:00Z and review before extending.\n").await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivery_response_requires_recipient_success_and_never_exposes_error_body() {
        assert_eq!(parse_response(br#"{"success":true,"result":{"message_id":"test-id","delivered":["kerry@kerryhatcher.com"],"queued":[],"permanent_bounces":[]}}"#).unwrap().delivery,Delivery::Delivered);
        assert_eq!(parse_response(br#"{"success":true,"result":{"message_id":"test-id","delivered":[],"queued":["kerry@kerryhatcher.com"],"permanent_bounces":[]}}"#).unwrap().delivery,Delivery::Queued);
        let error = parse_response(
            br#"{"success":false,"errors":[{"message":"private-token"}],"result":null}"#,
        )
        .unwrap_err();
        assert!(!error.to_string().contains("private-token"));
        assert!(parse_response(br#"{"success":true,"result":{"message_id":"test-id","delivered":[],"queued":[],"permanent_bounces":["kerry@kerryhatcher.com"]}}"#).is_err());
    }
    #[tokio::test]
    async fn actual_transport_pins_sender_recipient_and_rejects_redirects_and_large_responses() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for (status, extra, body, expected) in [
            (
                "200 OK",
                "",
                r#"{"success":true,"result":{"message_id":"accepted-id","delivered":["kerry@kerryhatcher.com"],"queued":[],"permanent_bounces":[]}}"#,
                true,
            ),
            (
                "302 Found",
                "Location: https://credentials.example/secret\r\n",
                "",
                false,
            ),
            ("200 OK", "Content-Length: 1000000\r\n", "", false),
            (
                "500 Internal Server Error",
                "",
                "private-token-error",
                false,
            ),
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
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            let payload: serde_json::Value =
                                serde_json::from_slice(&bytes[end + 4..]).unwrap();
                            assert_eq!(payload["from"]["address"], FROM);
                            assert_eq!(payload["to"], RECIPIENT);
                            assert!(headers.contains("authorization: bearer test-token"));
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
            let result = CloudflareMail::new("test-token")
                .unwrap()
                .request(&url, "CivicPulse test: verification", "Safe alert text")
                .await;
            assert_eq!(result.is_ok(), expected);
            if let Err(error) = result {
                assert!(!error.to_string().contains("private-token"));
                assert!(!error.to_string().contains("credentials.example"));
            }
            server.await.unwrap();
        }
    }
}
