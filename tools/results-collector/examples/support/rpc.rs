use civpulse_results_collector::{Error, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
pub const ELECTION: &str = "00000000-0000-4000-8000-000000000001";
pub const SNAPSHOT: &str = "00000000-0000-4000-8000-000000000002";
pub const SOURCE: &str =
    "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json";
#[derive(Default)]
pub struct State {
    pub claimed: bool,
    pub staged: usize,
    pub batches: usize,
    pub largest_batch: usize,
    pub published: bool,
    pub completed: bool,
    pub fingerprint: String,
}
pub async fn start(
    receipt: Value,
    election_date: String,
    state: Arc<Mutex<State>>,
) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| Error::Network)?;
    let url = format!(
        "http://{}",
        listener.local_addr().map_err(|_| Error::Network)?
    );
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let receipt = receipt.clone();
            let date = election_date.clone();
            let state = state.clone();
            tokio::spawn(async move {
                let _ = request(stream, receipt, date, state).await;
            });
        }
    });
    Ok((url, task))
}
async fn request(
    mut stream: TcpStream,
    receipt: Value,
    date: String,
    state: Arc<Mutex<State>>,
) -> Result<()> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let (end, length) = loop {
        let n = stream.read(&mut buffer).await.map_err(|_| Error::Network)?;
        if n == 0 || bytes.len() + n > 3 * 1024 * 1024 {
            return Err(Error::ResponseLimit);
        }
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end])
                .map_err(|_| Error::InvalidResponse)?
                .to_lowercase();
            let length = header
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|n| n.parse::<usize>().ok())
                .ok_or(Error::InvalidResponse)?;
            if bytes.len() >= end + 4 + length {
                break (end, length);
            }
        }
    };
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| Error::InvalidResponse)?;
    let name = header
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|p| p.strip_prefix("/rest/v1/rpc/"))
        .ok_or(Error::InvalidResponse)?;
    let args: Value = serde_json::from_slice(&bytes[end + 4..end + 4 + length])
        .map_err(|_| Error::InvalidResponse)?;
    let mut s = state.lock().await;
    let value = match name {
        "claim_sos_results_job" if !s.claimed => {
            s.claimed = true;
            json!({"job_id":"00000000-0000-4000-8000-000000000003","lease_id":"00000000-0000-4000-8000-000000000004","election_id":ELECTION,"election_date":date,"source_url":SOURCE,"phase":"import","snapshot_id":SNAPSHOT,"receipt":receipt})
        }
        "claim_sos_results_job" => Value::Null,
        "renew_sos_results_job" => json!(true),
        "begin_sos_results_job" => {
            json!({"snapshot_id":SNAPSHOT,"receipt":receipt,"published":false})
        }
        "stage_sos_results_job" => {
            let items = args["p_items"].as_array().ok_or(Error::InvalidResponse)?;
            if items.len() > 50 || length > 2 * 1024 * 1024 + 4096 {
                return Err(Error::ResponseLimit);
            }
            s.staged += items.len();
            s.batches += 1;
            s.largest_batch = s.largest_batch.max(length);
            Value::Null
        }
        "publish_sos_results_job" => {
            s.published = true;
            s.fingerprint = args["p_fingerprint"]
                .as_str()
                .ok_or(Error::InvalidResponse)?
                .into();
            json!("9007199254740993")
        }
        "confirm_sos_results_pointer" if s.published => {
            json!({"revision":"9007199254740993","snapshot_id":SNAPSHOT,"manifest_key":format!("elections/{ELECTION}/revisions/{SNAPSHOT}/manifest.json"),"published_at":"2026-11-03T10:00:20Z"})
        }
        "complete_sos_results_pointer" if s.published => {
            s.completed = true;
            Value::Null
        }
        "fail_sos_results_job" => {
            return Err(Error::Storage("Benchmark worker reported durable failure"));
        }
        _ => return Err(Error::InvalidResponse),
    };
    drop(s);
    let body = serde_json::to_vec(&value).map_err(|_| Error::InvalidResponse)?;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|_| Error::Network)?;
    stream.write_all(&body).await.map_err(|_| Error::Network)?;
    Ok(())
}
