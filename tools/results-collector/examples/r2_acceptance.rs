//! Explicit opt-in S3 acceptance against the isolated test bucket only.
use civpulse_results_collector::{
    Result,
    storage::{Conditions, POINTER_CACHE, R2Store, Store, immutable, publish_pointer},
};
use serde_json::json;
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("SOS_R2_TEST_ACCEPTANCE").as_deref() != Ok("true") {
        return Err(civpulse_results_collector::Error::Config(
            "Set SOS_R2_TEST_ACCEPTANCE=true for isolated test-bucket writes",
        ));
    }
    let env = |key| {
        std::env::var(key)
            .map_err(|_| civpulse_results_collector::Error::Config("Missing test R2 credential"))
    };
    let store = R2Store::new(
        "test",
        &env("R2_ENDPOINT")?,
        "civpulse-files-test",
        "https://files-test.civpulse.org",
        &env("R2_ACCESS_KEY_ID")?,
        &env("R2_SECRET_ACCESS_KEY")?,
    )?;
    let election = "00000000-0000-4000-8000-000000000004";
    let key = format!("elections/{election}/raw/acceptance-v1.json");
    immutable(&store, &key, b"{\"synthetic\":true}").await?;
    immutable(&store, &key, b"{\"synthetic\":true}").await?;
    assert!(
        immutable(&store, &key, b"{\"synthetic\":false}")
            .await
            .is_err()
    );
    let pointer = |revision| json!({"schema_version":1,"election_id":election,"snapshot_id":"00000000-0000-4000-8000-000000000004","revision":revision});
    let status = publish_pointer(&store, &pointer("2"), || async { Ok(true) }).await?;
    assert!(matches!(status, "published" | "current"));
    assert_eq!(
        publish_pointer(&store, &pointer("1"), || async { Ok(true) }).await?,
        "superseded"
    );
    assert_eq!(
        publish_pointer(&store, &pointer("2"), || async { Ok(true) }).await?,
        "current"
    );
    let got = store
        .get(&format!("elections/{election}/latest.json"), 1024)
        .await?;
    assert!(got.etag.is_some());
    let key = format!("elections/{election}/latest.json");
    assert_eq!(
        store
            .put(
                &key,
                &got.body,
                Conditions {
                    if_match: Some("\"deliberately-wrong-etag\"".into()),
                    cache_control: POINTER_CACHE,
                    ..Conditions::default()
                }
            )
            .await?,
        412
    );
    let replaced = store
        .put(
            &key,
            &got.body,
            Conditions {
                if_match: got.etag,
                cache_control: POINTER_CACHE,
                ..Conditions::default()
            },
        )
        .await?;
    assert!((200..300).contains(&replaced));
    assert_eq!(store.get(&key, 1024).await?.body, got.body);
    println!("isolated R2 immutable checksum/create-only and pointer CAS acceptance passed");
    Ok(())
}
