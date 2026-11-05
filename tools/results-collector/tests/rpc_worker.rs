use civpulse_results_collector::{
    Error,
    config::Config,
    job::{Job, JobFuture, JobProcessor, Lease, UnsupportedProcessor},
    rpc::Rpc,
    worker::{self, Options},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, watch},
};

async fn server(
    handler: impl Fn(String) -> (String, Duration) + Send + Sync + 'static,
) -> (
    String,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 2048];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|n| n.parse::<usize>().ok())
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            let (response, delay) = handler(request.clone());
            let _ = tx.send(request);
            tokio::time::sleep(delay).await;
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (url, rx, task)
}
fn response(body: &str) -> (String, Duration) {
    (
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        Duration::ZERO,
    )
}
fn client(url: &str) -> Rpc {
    Rpc::new(url, "secret-not-for-logs", Duration::from_secs(2), 4096).unwrap()
}
fn job() -> String {
    json!({"job_id":"00000000-0000-0000-0000-000000000001","lease_id":"00000000-0000-0000-0000-000000000002","election_id":"00000000-0000-0000-0000-000000000003","election_date":"2026-11-03","source_url":"https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json","phase":"import","etag":null,"last_modified":null,"snapshot_id":null,"revision":null,"manifest_key":null,"receipt":null}).to_string()
}
#[tokio::test]
async fn actual_rpc_headers_body_and_empty_queue() {
    let (url, mut requests, task) = server(|_| response("null")).await;
    let options = Options {
        enabled: true,
        ..Options::default()
    };
    assert!(
        !worker::once(&client(&url), Arc::new(UnsupportedProcessor), &options)
            .await
            .unwrap()
    );
    let request = requests.recv().await.unwrap();
    assert!(request.starts_with("POST /rest/v1/rpc/claim_sos_results_job HTTP/1.1"));
    let lower = request.to_lowercase();
    assert!(lower.contains("apikey: secret-not-for-logs\r\n"));
    assert!(lower.contains("authorization: bearer secret-not-for-logs\r\n"));
    assert!(lower.contains("content-type: application/json\r\n"));
    assert!(request.ends_with("\r\n\r\n{}"));
    task.abort();
}
#[tokio::test]
async fn redirects_are_not_followed_and_errors_are_sanitized() {
    let (url, mut requests, task) = server(|_| ("HTTP/1.1 302 Found\r\nLocation: https://credentials.example/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), Duration::ZERO)).await;
    let error = client(&url)
        .call::<Value>("get_sos_collection_health", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error, Error::Http(302));
    assert!(!error.to_string().contains("secret"));
    requests.recv().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), requests.recv())
            .await
            .is_err()
    );
    task.abort();
}
#[tokio::test]
async fn response_limit_applies_without_content_length() {
    let (url, _, task) = server(|_| {
        (
            format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}",
                " ".repeat(5000)
            ),
            Duration::ZERO,
        )
    })
    .await;
    assert_eq!(
        client(&url)
            .call::<Value>("health", json!({}))
            .await
            .unwrap_err(),
        Error::ResponseLimit
    );
    task.abort();
}
#[tokio::test]
async fn deadline_covers_response() {
    let (url, _, task) = server(|_| {
        let (text, _) = response("null");
        (text, Duration::from_secs(1))
    })
    .await;
    let rpc = Rpc::new(&url, "key", Duration::from_millis(20), 4096).unwrap();
    assert_eq!(
        rpc.call::<Value>("health", json!({})).await.unwrap_err(),
        Error::Timeout
    );
    task.abort();
}
#[tokio::test]
async fn unsupported_processor_fails_durable_job_without_success() {
    let (url, mut requests, task) = server(|request| {
        if request.contains("/claim_sos_results_job ") {
            response(&job())
        } else {
            response("null")
        }
    })
    .await;
    let options = Options {
        enabled: true,
        ..Options::default()
    };
    assert_eq!(
        worker::once(&client(&url), Arc::new(UnsupportedProcessor), &options)
            .await
            .unwrap_err(),
        Error::Unsupported
    );
    requests.recv().await.unwrap();
    let failure = requests.recv().await.unwrap();
    assert!(failure.contains("/fail_sos_results_job "));
    let args: Value = serde_json::from_str(failure.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(args["p_job_id"], "00000000-0000-0000-0000-000000000001");
    assert_eq!(args["p_lease_id"], "00000000-0000-0000-0000-000000000002");
    assert!(
        args["p_error"]
            .as_str()
            .unwrap()
            .contains("not implemented")
    );
    assert!(args["p_retry_after"].is_null());
    task.abort();
}
struct Pending(Arc<AtomicBool>);
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl JobProcessor for Pending {
    fn process<'a>(&'a self, _: &'a Job, _: Lease) -> JobFuture<'a> {
        Box::pin(async move {
            let _guard = Dropped(self.0.clone());
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn lease_loss_cancels_processing_without_followup_mutation() {
    let (url, mut requests, task) = server(|request| {
        if request.contains("/claim_sos_results_job ") {
            response(&job())
        } else {
            response("false")
        }
    })
    .await;
    let cancelled = Arc::new(AtomicBool::new(false));
    let options = Options {
        enabled: true,
        renew_interval: Duration::from_millis(10),
        ..Options::default()
    };
    assert_eq!(
        worker::once(
            &client(&url),
            Arc::new(Pending(cancelled.clone())),
            &options
        )
        .await
        .unwrap_err(),
        Error::LeaseLost
    );
    assert!(cancelled.load(Ordering::SeqCst));
    requests.recv().await.unwrap();
    assert!(
        requests
            .recv()
            .await
            .unwrap()
            .contains("/renew_sos_results_job ")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), requests.recv())
            .await
            .is_err()
    );
    task.abort();
}
#[tokio::test]
async fn disabled_daemon_is_live_and_never_calls_rpc() {
    let (url, mut requests, task) = server(|_| response("null")).await;
    let (tx, rx) = watch::channel(false);
    let path = std::env::temp_dir().join(format!("collector-heartbeat-{}", std::process::id()));
    let options = Options {
        heartbeat_path: Some(path.clone()),
        ..Options::default()
    };
    let daemon = tokio::spawn(worker::daemon(
        Some(client(&url)),
        Arc::new(UnsupportedProcessor),
        options,
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(30)).await;
    worker::liveness(&path).await.unwrap();
    assert!(requests.try_recv().is_err());
    tx.send(true).unwrap();
    daemon.await.unwrap().unwrap();
    tokio::fs::remove_file(path).await.unwrap();
    task.abort();
}
#[tokio::test]
async fn shutdown_drains_then_cancels_and_stops_renewing() {
    let renewals = Arc::new(AtomicUsize::new(0));
    let counts = renewals.clone();
    let (url, mut requests, task) = server(move |request| {
        if request.contains("/claim_sos_results_job ") {
            response(&job())
        } else if request.contains("/renew_sos_results_job ") {
            counts.fetch_add(1, Ordering::SeqCst);
            response("true")
        } else {
            // A cancelled connection can reach the listener without a request;
            // neither it nor the explicit health barrier is a lease renewal.
            response("[]")
        }
    })
    .await;
    let (tx, rx) = watch::channel(false);
    let cancelled = Arc::new(AtomicBool::new(false));
    let options = Options {
        enabled: true,
        drain_timeout: Duration::from_millis(50),
        renew_interval: Duration::from_millis(10),
        ..Options::default()
    };
    let daemon = tokio::spawn(worker::daemon(
        Some(client(&url)),
        Arc::new(Pending(cancelled.clone())),
        options,
        rx,
    ));
    requests.recv().await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    tx.send(true).unwrap();
    assert_eq!(daemon.await.unwrap().unwrap_err(), Error::Shutdown);
    assert!(cancelled.load(Ordering::SeqCst));
    // The server accepts connections sequentially. A read-only request acts as a
    // network barrier for renewals sent before cancellation but not yet handled;
    // cancellation cannot recall bytes already queued in the TCP listener.
    let health: Vec<Value> = client(&url)
        .call("get_sos_collection_health", json!({}))
        .await
        .unwrap();
    assert_eq!(health, Vec::<Value>::new());
    let count = renewals.load(Ordering::SeqCst);
    assert!(count > 0);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(renewals.load(Ordering::SeqCst), count);
    task.abort();
}
#[test]
fn configuration_is_disabled_by_default_and_bounded() {
    let config = Config::read(|_| None).unwrap();
    assert!(!config.enabled);
    assert_eq!(config.concurrency, 1);
    assert!(Config::read(|key| (key == "SOS_COLLECTOR_ENABLED").then(|| "TRUE".into())).is_err());
    assert!(Config::read(|key| (key == "SOS_COLLECTOR_CONCURRENCY").then(|| "5".into())).is_err());
    assert!(
        Rpc::new(
            "http://untrusted.example",
            "key",
            Duration::from_secs(1),
            4096
        )
        .is_err()
    );
    assert!(
        Rpc::new(
            "https://user:secret@example.com",
            "key",
            Duration::from_secs(1),
            4096
        )
        .is_err()
    );
}

#[derive(Clone, Copy)]
enum RetainMode {
    Success,
    Fail,
    Pending,
}
struct RetainLease {
    leases: mpsc::UnboundedSender<Lease>,
    mode: RetainMode,
}
impl JobProcessor for RetainLease {
    fn process<'a>(&'a self, _: &'a Job, lease: Lease) -> JobFuture<'a> {
        Box::pin(async move {
            self.leases.send(lease).unwrap();
            match self.mode {
                RetainMode::Success => Ok(()),
                RetainMode::Fail => Err(Error::Unsupported),
                RetainMode::Pending => std::future::pending().await,
            }
        })
    }
}

#[tokio::test]
async fn retained_leases_are_revoked_on_success_and_failure_without_losing_failure_report() {
    for mode in [RetainMode::Success, RetainMode::Fail] {
        let (url, mut requests, task) = server(|request| {
            if request.contains("/claim_sos_results_job ") {
                response(&job())
            } else {
                response("null")
            }
        })
        .await;
        let (leases, mut retained) = mpsc::unbounded_channel();
        let result = worker::once(
            &client(&url),
            Arc::new(RetainLease { leases, mode }),
            &Options {
                enabled: true,
                ..Options::default()
            },
        )
        .await;
        match mode {
            RetainMode::Success => assert_eq!(result, Ok(true)),
            RetainMode::Fail => assert_eq!(result, Err(Error::Unsupported)),
            RetainMode::Pending => unreachable!(),
        }
        assert!(
            requests
                .recv()
                .await
                .unwrap()
                .contains("/claim_sos_results_job ")
        );
        if matches!(mode, RetainMode::Fail) {
            assert!(
                requests
                    .recv()
                    .await
                    .unwrap()
                    .contains("/fail_sos_results_job ")
            );
        }
        let lease = retained.recv().await.unwrap();
        assert!(lease.is_lost());
        assert!(*lease.cancellation().borrow());
        assert_eq!(
            lease
                .call::<Value>("stage_sos_results_job", json!({}))
                .await,
            Err(Error::LeaseLost)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
}

#[tokio::test]
async fn forced_drain_revokes_retained_leases_and_cancels_waiting_mutation() {
    let (url, mut requests, task) = server(|request| {
        if request.contains("/claim_sos_results_job ") {
            response(&job())
        } else if request.contains("/stage_sos_results_job ") {
            let (body, _) = response("null");
            (body, Duration::from_secs(1))
        } else {
            response("true")
        }
    })
    .await;
    let (leases, mut retained) = mpsc::unbounded_channel();
    let (shutdown, receiver) = watch::channel(false);
    let daemon = tokio::spawn(worker::daemon(
        Some(client(&url)),
        Arc::new(RetainLease {
            leases,
            mode: RetainMode::Pending,
        }),
        Options {
            enabled: true,
            drain_timeout: Duration::from_millis(20),
            ..Options::default()
        },
        receiver,
    ));
    let lease = retained.recv().await.unwrap();
    let in_flight = lease.clone();
    let waiting = tokio::spawn(async move {
        in_flight
            .call::<Value>("stage_sos_results_job", json!({}))
            .await
    });
    assert!(
        requests
            .recv()
            .await
            .unwrap()
            .contains("/claim_sos_results_job ")
    );
    assert!(
        requests
            .recv()
            .await
            .unwrap()
            .contains("/stage_sos_results_job ")
    );
    shutdown.send(true).unwrap();
    assert_eq!(daemon.await.unwrap(), Err(Error::Shutdown));
    assert!(lease.is_lost());
    assert!(*lease.cancellation().borrow());
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(100), waiting)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::LeaseLost)
    );
    assert_eq!(
        lease
            .call::<Value>("stage_sos_results_job", json!({}))
            .await,
        Err(Error::LeaseLost)
    );
    task.abort();
}

#[tokio::test]
async fn actual_rpc_wire_uses_javascript_numbers_for_all_source_payload_fields() {
    let (url, mut requests, task) = server(|_| response("null")).await;
    let raw: Value = serde_json::from_str(r#"{"p_header":{"sourceSequence":9007199254740993,"negativeZero":-0.0},"p_jurisdictions":[{"unknownCounter":9007199254740993}],"p_items":[{"data":{"unknownCounter":9007199254740993}}],"p_revision":"9007199254740993"}"#).unwrap();
    client(&url)
        .call::<Value>("begin_sos_results_job", raw)
        .await
        .unwrap();
    let request = requests.recv().await.unwrap();
    assert!(
        request
            .to_lowercase()
            .contains("content-type: application/json\r\n")
    );
    // Expected bytes from the reference's JSON.stringify(JSON.parse(raw)). Decimal
    // revision strings retain every digit while source numbers use binary64.
    assert_eq!(
        request.split("\r\n\r\n").nth(1).unwrap(),
        r#"{"p_header":{"sourceSequence":9007199254740992,"negativeZero":0},"p_jurisdictions":[{"unknownCounter":9007199254740992}],"p_items":[{"data":{"unknownCounter":9007199254740992}}],"p_revision":"9007199254740993"}"#
    );
    task.abort();
}
#[tokio::test]
async fn rpc_wrapper_allowance_is_bounded_without_reducing_source_depth() {
    let (url, mut requests, task) = server(|_| response("null")).await;
    let mut source = Value::Null;
    for _ in 0..40 {
        source = json!([source]);
    }
    assert!(civpulse_results_source::stringify(&source).is_ok());
    let args = json!({"p_header":source});
    assert!(civpulse_results_source::stringify(&args).is_err());
    client(&url)
        .call::<Value>("begin_sos_results_job", args)
        .await
        .unwrap();
    requests.recv().await.unwrap();
    let mut too_deep = Value::Null;
    for _ in 0..45 {
        too_deep = json!([too_deep]);
    }
    assert_eq!(
        client(&url)
            .call::<Value>("begin_sos_results_job", too_deep)
            .await
            .unwrap_err(),
        Error::InvalidResponse
    );
    assert!(requests.try_recv().is_err());
    assert!(civpulse_results_source::stringify_envelope(&Value::Null, 5).is_err());
    task.abort();
}

#[test]
fn deployment_rpc_origins_are_bound_to_the_matching_environment() {
    let dev = "http://supabase-supabase-kong.civpulse-dev.svc.cluster.local:8000";
    let test = "http://supabase-supabase-kong.civpulse-test.svc.cluster.local:8000";
    let build = |environment: &str, origin: &str| {
        Rpc::for_environment(environment, origin, "fixture", Duration::from_secs(1), 4096)
    };
    assert!(build("dev", dev).is_ok());
    assert!(build("test", test).is_ok());
    assert!(build("test", &format!("{test}/")).is_ok());
    for environment in ["dev", "test", "prod"] {
        assert!(build(environment, "https://project.supabase.co").is_ok());
        for forbidden in [
            "http://127.0.0.1:54321",
            "http://localhost:8000",
            "http://supabase-supabase-kong.civpulse-prod.svc.cluster.local:8000",
            "http://supabase-supabase-kong.civpulse-test.svc.cluster.local:8001",
            "http://another-service.civpulse-test.svc.cluster.local:8000",
            "http://supabase-supabase-kong.civpulse-test.svc.cluster.local:8000.evil.example",
            "https://user:secret@project.supabase.co",
            "https://project.supabase.co/prefix",
            "https://project.supabase.co?token=secret",
        ] {
            assert!(
                build(environment, forbidden).is_err(),
                "{environment}: {forbidden}"
            );
        }
    }
    assert!(build("prod", dev).is_err());
    assert!(build("prod", test).is_err());
    assert!(build("dev", test).is_err());
    assert!(build("test", dev).is_err());
    assert!(build("unknown", "https://project.supabase.co").is_err());
    assert!(build("test", &format!("{test}//")).is_err());
    assert!(build("test", &format!("{test}/rest/v1")).is_err());
    assert!(build("test", &format!("{test}?token=secret")).is_err());
    assert!(build("test", &format!("{test}#fragment")).is_err());
    assert!(Config::read(|key| (key == "CIVPULSE_ENV").then(|| "staging".into())).is_err());
    assert_eq!(Config::read(|_| None).unwrap().environment, "prod");
}
