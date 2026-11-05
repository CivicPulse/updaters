//! Exercise the benchmark doubles against the real Publisher using disk-backed bytes.
#[path = "../examples/support/disk.rs"]
mod disk;
#[path = "../examples/support/rpc.rs"]
mod local_rpc;
use civpulse_results_collector::{
    pipeline::Publisher,
    rpc::Rpc,
    storage::{Conditions, Store},
    worker::{self, Options},
};
use civpulse_results_source::Expectations;
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;
#[tokio::test]
async fn offline_disk_pipeline_publishes_complete_snapshot_and_fenced_pointer() {
    let scratch = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/civpulse-results-contract/tests/fixtures/base.json");
    let (hash, bytes) = disk::digest(&source).await.unwrap();
    let store = Arc::new(disk::DiskStore::new(scratch.path().join("objects")));
    let key = format!("elections/{}/raw/{hash}.json", local_rpc::ELECTION);
    assert_eq!(
        store
            .put_file(
                &key,
                &source,
                &hash,
                bytes,
                Conditions {
                    if_none_match: true,
                    ..Conditions::default()
                }
            )
            .await
            .unwrap(),
        200
    );
    let receipt = json!({"source_url":local_rpc::SOURCE,"raw_key":key,"raw_url":store.url(&key).unwrap(),"sha256":hash,"bytes":bytes,"retrieved_at":"2026-11-03T10:00:10Z","etag":null,"last_modified":null});
    let state = Arc::new(Mutex::new(local_rpc::State::default()));
    let (url, server) = local_rpc::start(receipt, "2026-11-03".into(), state.clone())
        .await
        .unwrap();
    let publisher = Publisher::new(
        store.clone(),
        uuid::Uuid::parse_str(local_rpc::ELECTION).unwrap(),
        Expectations {
            election_date: "2026-11-03".into(),
            expected_jurisdictions: Some(2),
            expected_items: Some(2),
            accepted_topologies: vec![],
            topology_policy: None,
        },
        local_rpc::SOURCE.into(),
        scratch.path().join("spool"),
    );
    let rpc = Rpc::new(
        &url,
        "offline-test-not-a-secret",
        Duration::from_secs(2),
        1024 * 1024,
    )
    .unwrap();
    assert!(
        worker::once(
            &rpc,
            Arc::new(publisher),
            &Options {
                enabled: true,
                ..Options::default()
            }
        )
        .await
        .unwrap()
    );
    server.abort();
    let state = state.lock().await;
    assert!(state.completed);
    assert_eq!(state.staged, 2);
    assert_eq!(
        state.fingerprint,
        "a4d3834c495de3b43ec8e04284dbc39ff123e93a8baf73148eb5bbe659b10c67"
    );
    assert!(state.largest_batch <= 2 * 1024 * 1024 + 4096);
    let pointer = format!("elections/{}/latest.json", local_rpc::ELECTION);
    let current = store.get(&pointer, 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&current.body).unwrap();
    assert_eq!(value["revision"], "9007199254740993");
    assert_eq!(value["jurisdictions"].as_array().unwrap().len(), 2);
    assert_eq!(
        store
            .put(
                &pointer,
                b"stale",
                Conditions {
                    if_match: Some("wrong-etag".into()),
                    ..Conditions::default()
                }
            )
            .await
            .unwrap(),
        412
    );
    assert_eq!(
        store.get(&pointer, 1024 * 1024).await.unwrap().body,
        current.body
    );
    assert!(store.get(&key, 1024).await.is_err());
}
