use async_trait::async_trait;
use civpulse_results_collector::{
    Error, Result,
    storage::{
        Conditions, R2Store, Store, Stored, immutable, publish_pointer, sha256, validate_key,
    },
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
const ELECTION: &str = "00000000-0000-4000-8000-000000000001";
fn pointer(revision: &str, snapshot: u8) -> Value {
    json!({"schema_version":1,"election_id":ELECTION,"snapshot_id":format!("00000000-0000-4000-8000-{snapshot:012}"),"revision":revision})
}
#[derive(Default)]
struct Memory {
    data: Mutex<HashMap<String, (Vec<u8>, u64)>>,
    fail: AtomicBool,
    race: Mutex<Option<Value>>,
}
#[async_trait]
impl Store for Memory {
    fn url(&self, key: &str) -> Result<String> {
        Ok(format!("https://files-test.civpulse.org/{key}"))
    }
    async fn get(&self, key: &str, _: usize) -> Result<Stored> {
        let data = self.data.lock().unwrap();
        Ok(match data.get(key) {
            Some((body, n)) => Stored {
                status: 200,
                body: body.clone(),
                etag: Some(format!("\"{n}\"")),
            },
            None => Stored {
                status: 404,
                body: vec![],
                etag: None,
            },
        })
    }
    async fn put(&self, key: &str, bytes: &[u8], c: Conditions) -> Result<u16> {
        if self.fail.load(Ordering::Relaxed) {
            return Ok(503);
        }
        let mut data = self.data.lock().unwrap();
        if let Some(race) = self.race.lock().unwrap().take() {
            data.insert(key.into(), (serde_json::to_vec(&race).unwrap(), 100));
        }
        let prior = data.get(key);
        if c.if_none_match && prior.is_some()
            || c.if_match
                .as_ref()
                .is_some_and(|m| prior.is_none_or(|(_, n)| *m != format!("\"{n}\"")))
        {
            return Ok(412);
        }
        let n = prior.map_or(1, |(_, n)| n + 1);
        data.insert(key.into(), (bytes.into(), n));
        Ok(200)
    }
    async fn put_file(
        &self,
        key: &str,
        path: &Path,
        _: &str,
        _: u64,
        c: Conditions,
    ) -> Result<u16> {
        self.put(key, &std::fs::read(path).unwrap(), c).await
    }
    async fn download(&self, key: &str, path: &Path, hash: &str, bytes: u64) -> Result<()> {
        self.verify(key, hash, bytes).await?;
        std::fs::write(path, self.get(key, usize::MAX).await?.body).unwrap();
        Ok(())
    }
    async fn verify(&self, key: &str, hash: &str, bytes: u64) -> Result<()> {
        let value = self.get(key, usize::MAX).await?;
        if value.status != 200 || sha256(&value.body) != hash || value.body.len() as u64 != bytes {
            return Err(Error::Storage("test checksum mismatch"));
        }
        Ok(())
    }
}
struct Golden(Memory);
#[async_trait]
impl Store for Golden {
    fn url(&self, key: &str) -> Result<String> {
        Ok(format!("https://files.civpulse.org/{key}"))
    }
    async fn get(&self, key: &str, limit: usize) -> Result<Stored> {
        self.0.get(key, limit).await
    }
    async fn put(&self, key: &str, bytes: &[u8], c: Conditions) -> Result<u16> {
        self.0.put(key, bytes, c).await
    }
    async fn put_file(
        &self,
        key: &str,
        path: &Path,
        hash: &str,
        size: u64,
        c: Conditions,
    ) -> Result<u16> {
        self.0.put_file(key, path, hash, size, c).await
    }
    async fn verify(&self, key: &str, hash: &str, size: u64) -> Result<()> {
        self.0.verify(key, hash, size).await
    }
    async fn download(&self, key: &str, path: &Path, hash: &str, size: u64) -> Result<()> {
        self.0.download(key, path, hash, size).await
    }
}
#[tokio::test]
async fn immutable_collision_and_failed_upload_preserve_objects() {
    let store = Memory::default();
    let key = format!("elections/{ELECTION}/raw/a.json");
    immutable(&store, &key, b"original").await.unwrap();
    immutable(&store, &key, b"original").await.unwrap();
    assert!(immutable(&store, &key, b"correction").await.is_err());
    assert_eq!(store.get(&key, 100).await.unwrap().body, b"original");
    store.fail.store(true, Ordering::Relaxed);
    assert!(immutable(&store, "new", b"data").await.is_err());
}
#[tokio::test]
async fn crash_after_pointer_put_is_acknowledged_without_rewrite() {
    let store = Memory::default();
    let p = pointer("9007199254740993", 2);
    assert_eq!(
        publish_pointer(&store, &p, || async { Ok(true) })
            .await
            .unwrap(),
        "published"
    );
    let before = store
        .get(&format!("elections/{ELECTION}/latest.json"), 1000)
        .await
        .unwrap()
        .etag;
    assert_eq!(
        publish_pointer(&store, &p, || async {
            panic!("no reconfirm needed for same committed pointer")
        })
        .await
        .unwrap(),
        "current"
    );
    assert_eq!(
        before,
        store
            .get(&format!("elections/{ELECTION}/latest.json"), 1000)
            .await
            .unwrap()
            .etag
    );
}
#[tokio::test]
async fn delayed_worker_and_equal_revision_conflict_cannot_regress_pointer() {
    let store = Memory::default();
    let current = pointer("100", 2);
    publish_pointer(&store, &current, || async { Ok(true) })
        .await
        .unwrap();
    assert_eq!(
        publish_pointer(&store, &pointer("99", 3), || async { Ok(true) })
            .await
            .unwrap(),
        "superseded"
    );
    assert!(
        publish_pointer(&store, &pointer("100", 3), || async { Ok(true) })
            .await
            .is_err()
    );
    assert_eq!(
        publish_pointer(&store, &pointer("101", 3), || async { Ok(false) })
            .await
            .unwrap(),
        "superseded"
    );
    let actual: Value = serde_json::from_slice(
        &store
            .get(&format!("elections/{ELECTION}/latest.json"), 1000)
            .await
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(actual, current);
}
#[tokio::test]
async fn concurrent_newer_pointer_wins_atomic_create_and_replace() {
    for existing in [false, true] {
        let store = Memory::default();
        if existing {
            publish_pointer(&store, &pointer("1", 1), || async { Ok(true) })
                .await
                .unwrap();
        }
        *store.race.lock().unwrap() = Some(pointer("3", 3));
        assert_eq!(
            publish_pointer(&store, &pointer("2", 2), || async { Ok(true) })
                .await
                .unwrap(),
            "superseded"
        );
    }
}
#[test]
fn environments_cannot_cross_buckets_or_origins_and_paths_are_safe() {
    let endpoint = "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";
    assert!(
        R2Store::new(
            "test",
            endpoint,
            "civpulse-files-test",
            "https://files-test.civpulse.org",
            "a",
            "b"
        )
        .is_ok()
    );
    assert!(
        R2Store::new(
            "test",
            endpoint,
            "civpulse-files",
            "https://files.civpulse.org",
            "a",
            "b"
        )
        .is_err()
    );
    assert!(
        R2Store::new(
            "dev",
            endpoint,
            "civpulse-files-dev",
            "https://files.civpulse.org",
            "a",
            "b"
        )
        .is_err()
    );
    for key in [
        "elections/bad/raw/a",
        "elections/00000000-0000-4000-8000-000000000001/raw/../private",
        "elections/00000000-0000-4000-8000-000000000001/raw/%2e",
        "elections/00000000-0000-4000-8000-000000000001/raw//a",
    ] {
        assert!(validate_key(key).is_err());
    }
}
#[tokio::test]
async fn rust_assets_match_pinned_reference_exact_bytes() {
    use civpulse_results_collector::pipeline::write_assets;
    use civpulse_results_source::{Expectations, Limits, prepare};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/civpulse-results-contract/tests/fixtures");
    let dir = tempfile::tempdir().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let prepared = prepare(
        &root.join("base.json"),
        &dir.path().join("scopes"),
        &Expectations {
            election_date: "2026-11-03".into(),
            expected_jurisdictions: Some(2),
            expected_items: Some(2),
        },
        &Limits::default(),
        &cancel,
    )
    .unwrap();
    let latest: Value =
        serde_json::from_slice(&std::fs::read(root.join("latest.json")).unwrap()).unwrap();
    let receipt = json!({"source_url":latest["source_url"],"sha256":latest["source_sha256"],"bytes":latest["source_bytes"],"raw_url":latest["raw_url"],"retrieved_at":latest["retrieved_at"],"etag":latest["etag"],"last_modified":latest["last_modified"]});
    let store = Memory::default();
    // The golden oracle uses production URLs, but no network or production bucket writes occur.
    let store = Golden(store);
    write_assets(
        &store,
        ELECTION,
        "00000000-0000-4000-8000-000000000002",
        &prepared,
        &receipt,
        &cancel,
    )
    .await
    .unwrap();
    let expected: Value =
        serde_json::from_slice(&std::fs::read(root.join("expected.json")).unwrap()).unwrap();
    for asset in expected["assets"].as_array().unwrap() {
        let actual = store
            .get(asset["key"].as_str().unwrap(), usize::MAX)
            .await
            .unwrap()
            .body;
        let expected = std::fs::read(root.join(asset["file"].as_str().unwrap())).unwrap();
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            String::from_utf8(expected).unwrap(),
            "{}",
            asset["file"]
        );
    }
}
mod pipeline_scenarios {
    use super::*;
    use civpulse_results_collector::{
        pipeline::Publisher,
        rpc::Rpc,
        worker::{self, Options},
    };
    use civpulse_results_source::Expectations;
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    const SNAPSHOT: &str = "00000000-0000-4000-8000-000000000002";
    const SOURCE: &str = "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json";
    struct State {
        job: Mutex<Value>,
        calls: Mutex<Vec<String>>,
        fault: Mutex<Option<&'static str>>,
        store: Arc<Memory>,
        receipt: Value,
    }
    #[allow(clippy::too_many_lines)] // One explicit fault-injected RPC state machine per recovery scenario.
    async fn run(fault: &'static str) {
        let raw = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/civpulse-results-contract/tests/fixtures/base.json"),
        )
        .unwrap();
        let hash = sha256(&raw);
        let raw_key = format!("elections/{ELECTION}/raw/{hash}.json");
        let store = Arc::new(Memory::default());
        immutable(store.as_ref(), &raw_key, &raw).await.unwrap();
        let receipt = json!({"source_url":SOURCE,"raw_key":raw_key,"raw_url":store.url(&raw_key).unwrap(),"sha256":hash,"bytes":raw.len(),"retrieved_at":"2026-11-03T10:00:10Z","etag":null,"last_modified":null});
        let job = json!({"job_id":"00000000-0000-4000-8000-000000000011","lease_id":"00000000-0000-4000-8000-000000000012","election_id":ELECTION,"election_date":"2026-11-03","source_url":SOURCE,"phase":"import","snapshot_id":SNAPSHOT,"receipt":receipt});
        let state = Arc::new(State {
            job: Mutex::new(job),
            calls: Mutex::new(vec![]),
            fault: Mutex::new(Some(fault)),
            store: store.clone(),
            receipt,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![];
                let mut buf = [0; 4096];
                let end;
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(p) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..p]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= p + 4 + length {
                            end = p + 4;
                            break;
                        }
                    }
                }
                let route = String::from_utf8_lossy(&bytes[..end])
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned();
                let args: Value = serde_json::from_slice(&bytes[end..]).unwrap();
                server_state.calls.lock().unwrap().push(route.clone());
                let (status, response) = {
                    let mut status = 200;
                    let mut fault = server_state.fault.lock().unwrap();
                    let response = match route.as_str() {
                        "claim_sos_results_job" => server_state.job.lock().unwrap().clone(),
                        "renew_sos_results_job" => json!(true),
                        "begin_sos_results_job" => {
                            assert_eq!(args["p_receipt"], server_state.receipt);
                            json!({"snapshot_id":SNAPSHOT,"published":false,"receipt":server_state.receipt})
                        }
                        "stage_sos_results_job" => {
                            assert_eq!(args["p_snapshot_id"], SNAPSHOT);
                            assert_ne!(args["p_items"].as_array().unwrap().len(), 0);
                            if *fault == Some("stage") {
                                *fault = None;
                                status = 500;
                            }
                            if *fault == Some("assets") {
                                *fault = None;
                                server_state.store.fail.store(true, Ordering::Relaxed);
                            }
                            Value::Null
                        }
                        "publish_sos_results_job" => {
                            server_state.job.lock().unwrap()["phase"] = json!("pointer");
                            if *fault == Some("commit") {
                                *fault = None;
                                status = 500;
                            }
                            json!("2")
                        }
                        "confirm_sos_results_pointer" => {
                            json!({"snapshot_id":SNAPSHOT,"manifest_key":format!("elections/{ELECTION}/revisions/{SNAPSHOT}/manifest.json"),"revision":"2","published_at":"2026-11-03T10:00:20Z"})
                        }
                        "complete_sos_results_pointer" => {
                            if *fault == Some("ack") {
                                *fault = None;
                                status = 500;
                            }
                            Value::Null
                        }
                        "fail_sos_results_job" => Value::Null,
                        other => panic!("unexpected RPC {other}"),
                    };
                    (status, response)
                };
                let body = response.to_string();
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let spool = tempfile::tempdir().unwrap();
        let publisher = Arc::new(Publisher::new(
            store.clone(),
            uuid::Uuid::parse_str(ELECTION).unwrap(),
            Expectations {
                election_date: "2026-11-03".into(),
                expected_jurisdictions: Some(2),
                expected_items: Some(2),
            },
            SOURCE.into(),
            spool.path().into(),
        ));
        let rpc = Rpc::new(&origin, "test-service", Duration::from_secs(5), 1024 * 1024).unwrap();
        let options = Options {
            enabled: true,
            ..Options::default()
        };
        assert!(
            worker::once(&rpc, publisher.clone(), &options)
                .await
                .is_err(),
            "fault {fault}"
        );
        let pointer_key = format!("elections/{ELECTION}/latest.json");
        let prior = store.get(&pointer_key, 4096).await.unwrap();
        if fault == "stage" || fault == "assets" {
            assert_eq!(prior.status, 404);
            assert!(
                !state
                    .calls
                    .lock()
                    .unwrap()
                    .contains(&"publish_sos_results_job".into())
            );
        }
        let stages_before = state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.as_str() == "stage_sos_results_job")
            .count();
        store.fail.store(false, Ordering::Relaxed);
        worker::once(&rpc, publisher, &options).await.unwrap();
        let actual = store.get(&pointer_key, 4096).await.unwrap();
        assert_eq!(actual.status, 200);
        let pointer: Value = serde_json::from_slice(&actual.body).unwrap();
        assert_eq!(pointer["revision"], "2");
        if fault == "commit" || fault == "ack" {
            assert_eq!(
                stages_before,
                state
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|v| v.as_str() == "stage_sos_results_job")
                    .count()
            );
        }
        if fault == "ack" {
            assert_eq!(prior.etag, actual.etag);
        }
        assert_eq!(store.get(&raw_key, 4096).await.unwrap().body, raw);
        server.abort();
    }
    #[tokio::test]
    async fn interrupted_staging_resumes_original_archive_and_receipt() {
        run("stage").await;
    }
    #[tokio::test]
    async fn failed_asset_never_activates_incomplete_snapshot() {
        run("assets").await;
    }
    #[tokio::test]
    async fn commit_crash_recovers_only_pointer_phase() {
        run("commit").await;
    }
    #[tokio::test]
    async fn pointer_ack_crash_does_not_reimport_or_rewrite() {
        run("ack").await;
    }
}
