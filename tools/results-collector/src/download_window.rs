//! Server-authorized fresh source reads; archived evidence does not need a new source window.
use crate::{
    Error, Result,
    job::{Job, Lease},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use tokio::time::Instant;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authorization {
    schema_version: u32,
    job_id: Uuid,
    election_id: Uuid,
    source_url: String,
    server_now: DateTime<Utc>,
    // Value makes the field required while preserving the legacy explicit-null end.
    ends_at: Value,
}

fn closed() -> Error {
    Error::Source("Fresh source polling window closed")
}

impl Authorization {
    fn deadline(self, job: &Job, started: Instant) -> Result<Option<Instant>> {
        if self.schema_version != 1
            || self.job_id != job.job_id
            || self.election_id != job.election_id
            || self.source_url != job.source_url
        {
            return Err(Error::InvalidResponse);
        }
        if self.ends_at.is_null() {
            // Existing unbounded feeds retain the source adapter's ordinary finite deadline.
            return Ok(None);
        }
        let end: DateTime<Utc> = self
            .ends_at
            .as_str()
            .ok_or(Error::InvalidResponse)?
            .parse()
            .map_err(|_| Error::InvalidResponse)?;
        let remaining = end
            .signed_duration_since(self.server_now)
            .to_std()
            .map_err(|_| closed())?
            .checked_sub(Duration::from_millis(1))
            .ok_or_else(closed)?;
        // Anchor at RPC start, not response receipt: this subtracts the entire
        // monotonic RPC round trip, including serialization/network/DB time.
        // A further millisecond covers timestamp precision differences. Worker
        // wall-clock skew cannot extend the server's finite window.
        let deadline = started
            .checked_add(remaining)
            .ok_or(Error::InvalidResponse)?;
        if deadline <= Instant::now() {
            return Err(closed());
        }
        Ok(Some(deadline))
    }
}

/// Reauthorize immediately before constructing a fresh source operation. The
/// absolute deadline covers request setup, headers and every streamed body byte.
pub(crate) async fn fetch<T, F, Fut>(job: &Job, lease: &Lease, operation: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let started = Instant::now();
    let authorization: Authorization = lease
        .call("authorize_sos_results_download", json!({}))
        .await?;
    if lease.is_lost() {
        return Err(Error::LeaseLost);
    }
    let Some(deadline) = authorization.deadline(job, started)? else {
        return operation().await;
    };
    tokio::select! {
        biased;
        () = tokio::time::sleep_until(deadline) => Err(closed()),
        result = async { operation().await } => {
            if Instant::now() >= deadline { Err(closed()) } else { result }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Rpc;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::watch,
    };

    fn job() -> Job {
        serde_json::from_value(json!({
            "job_id":"00000000-0000-4000-8000-000000000011",
            "lease_id":"00000000-0000-4000-8000-000000000012",
            "election_id":"00000000-0000-4000-8000-000000000001",
            "election_date":"2026-11-03", "phase":"import",
            "source_url":"https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json"
        })).unwrap()
    }
    fn permit(job: &Job, milliseconds: i64) -> Value {
        // Deliberately unrelated to the worker's real wall clock.
        let server_now: DateTime<Utc> = "2035-01-01T00:00:00Z".parse().unwrap();
        json!({"schema_version":1,"job_id":job.job_id,"election_id":job.election_id,
            "source_url":job.source_url,"server_now":server_now,
            "ends_at":server_now + chrono::Duration::milliseconds(milliseconds)})
    }
    async fn lease(
        job: &Job,
        status: u16,
        body: Value,
        delay: Duration,
    ) -> (Lease, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let expected = job.lease_args();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    assert!(
                        headers.starts_with("post /rest/v1/rpc/authorize_sos_results_download ")
                    );
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        assert_eq!(
                            serde_json::from_slice::<Value>(&bytes[end + 4..]).unwrap(),
                            expected
                        );
                        break;
                    }
                }
            }
            tokio::time::sleep(delay).await;
            let body = body.to_string();
            socket.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let rpc = Rpc::new(&origin, "test", Duration::from_secs(2), 4096).unwrap();
        let (sender, receiver) = watch::channel(false);
        (Lease::new(rpc, job.clone(), receiver), sender, server)
    }

    async fn rejected_without_fetch(status: u16, response: Value, delay: Duration) {
        let job = job();
        let (lease, _sender, server) = lease(&job, status, response, delay).await;
        let result = fetch::<(), _, _>(&job, &lease, || async {
            panic!("a rejected preflight must not construct or poll a fresh GET");
        })
        .await;
        assert!(result.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn closed_late_claim_and_full_round_trip_expiry_never_fetch() {
        rejected_without_fetch(403, json!({"message":"closed"}), Duration::ZERO).await;
        rejected_without_fetch(404, json!({"message":"missing RPC"}), Duration::ZERO).await;
        rejected_without_fetch(200, permit(&job(), 0), Duration::ZERO).await;
        rejected_without_fetch(200, permit(&job(), 1), Duration::ZERO).await;
        rejected_without_fetch(200, permit(&job(), -1000), Duration::ZERO).await;
        rejected_without_fetch(200, permit(&job(), 10), Duration::from_millis(30)).await;
    }

    #[tokio::test]
    async fn malformed_or_mismatched_authorization_never_fetches() {
        for (key, value) in [
            ("schema_version", json!(2)),
            ("job_id", json!(Uuid::nil())),
            ("election_id", json!(Uuid::nil())),
            ("source_url", json!("https://other.example/")),
            ("server_now", json!("invalid")),
            ("ends_at", json!("invalid")),
            ("ends_at", json!(42)),
        ] {
            let mut response = permit(&job(), 1000);
            response[key] = value;
            rejected_without_fetch(200, response, Duration::ZERO).await;
        }
        let mut missing_end = permit(&job(), 1000);
        missing_end.as_object_mut().unwrap().remove("ends_at");
        rejected_without_fetch(200, missing_end, Duration::ZERO).await;
        rejected_without_fetch(200, Value::Null, Duration::ZERO).await;
    }

    #[tokio::test]
    async fn server_clock_window_and_explicit_null_end_retain_legacy_deadline() {
        for null_end in [false, true] {
            let job = job();
            let mut response = permit(&job, 1000);
            if null_end {
                response["ends_at"] = Value::Null;
            }
            let (lease, _sender, server) = lease(&job, 200, response, Duration::ZERO).await;
            assert_eq!(fetch(&job, &lease, || async { Ok(42) }).await.unwrap(), 42);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn finite_end_cancels_an_in_progress_response_body_and_prevents_commit() {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", source.local_addr().unwrap());
        let disconnected = Arc::new(AtomicBool::new(false));
        let observed_close = disconnected.clone();
        let source_server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let mut request = Vec::new();
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), socket.read(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            observed_close.store(true, Ordering::SeqCst);
        });
        let job = job();
        let (lease, _sender, server) = lease(&job, 200, permit(&job, 5_000), Duration::ZERO).await;
        let committed = AtomicBool::new(false);
        let body_started = AtomicBool::new(false);
        let result = fetch(&job, &lease, || async {
            let mut response = reqwest::get(url).await.map_err(|_| Error::Network)?;
            while response
                .chunk()
                .await
                .map_err(|_| Error::Network)?
                .is_some()
            {
                body_started.store(true, Ordering::SeqCst);
            }
            committed.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(result, Err(closed()));
        assert!(body_started.load(Ordering::SeqCst));
        assert!(!committed.load(Ordering::SeqCst));
        server.await.unwrap();
        source_server.await.unwrap();
        assert!(disconnected.load(Ordering::SeqCst));
    }
}
