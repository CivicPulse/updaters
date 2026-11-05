use std::process::Command;
fn collector() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_civpulse-results-collector"));
    command.env_clear();
    command
}
#[test]
fn no_io_startup_and_unavailable_modes_are_honest() {
    assert!(collector().arg("--check").status().unwrap().success());
    assert!(collector().arg("--help").output().unwrap().status.success());
    for mode in ["once", "health", "validate", "liveness", "invalid"] {
        let output = collector().arg(mode).output().unwrap();
        assert!(!output.status.success(), "{mode} unexpectedly succeeded");
        assert_eq!(output.stdout, [] as [u8; 0]);
        assert_ne!(output.stderr, [] as [u8; 0]);
    }
}

fn source_fixture() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/civpulse-results-contract/tests/fixtures/base.json")
}

#[test]
fn local_validation_needs_no_credentials_and_returns_only_sanitized_summary() {
    for counts in [Vec::<&str>::new(), vec!["2", "2"]] {
        let output = collector()
            .arg("validate")
            .arg(source_fixture())
            .arg("2026-11-03")
            .args(counts)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stderr, [] as [u8; 0]);
        let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["event"], "source_validated");
        assert_eq!(summary["item_count"], 2);
        assert_eq!(summary["jurisdictions"], 2);
        let expected: serde_json::Value = serde_json::from_slice(
            &std::fs::read(source_fixture().with_file_name("expected.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            summary["fingerprint"],
            expected["cases"]["base"]["fingerprint"]
        );
        assert!(summary["duration_ms"].as_u64().is_some());
        let text = String::from_utf8(output.stdout).unwrap();
        for private in [
            "base.json",
            "Synthetic contract election",
            "Choice A",
            "SUPABASE",
            "00000000-",
        ] {
            assert!(!text.contains(private));
        }
    }
}

#[test]
fn local_validation_rejects_wrong_date_completeness_malformed_args_and_input() {
    let source = source_fixture();
    for tail in [
        vec!["2026-11-04"],
        vec!["invalid-date"],
        vec!["2026-11-03", "2", "3"],
        vec!["2026-11-03", "0", "2"],
        vec!["2026-11-03", "2", "bogus"],
        vec!["2026-11-03", "2"],
        vec!["2026-11-03", "2", "2", "extra"],
    ] {
        let output = collector()
            .arg("validate")
            .arg(&source)
            .args(tail)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, [] as [u8; 0]);
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("base.json")
        );
    }
    for args in [
        vec!["validate"],
        vec!["validate", "missing-path-secret"],
        vec!["invalid-mode"],
        vec!["health", "extra"],
    ] {
        let output = collector().args(args).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, [] as [u8; 0]);
    }
    let output = collector()
        .args(["validate", "missing-path-secret", "2026-11-03"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("missing-path-secret")
    );
    let temp = tempfile::tempdir().unwrap();
    let bad = temp.path().join("secret-source.json");
    std::fs::write(&bad, b"{not-json-private-data").unwrap();
    let output = collector()
        .arg("validate")
        .arg(&bad)
        .arg("2026-11-03")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!error.contains("secret-source") && !error.contains("not-json-private-data"));
}

#[cfg(unix)]
#[test]
fn sigterm_joins_active_parser_and_removes_task_owned_spool() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let temporary = tempfile::tempdir().unwrap();
    let scratch = temporary.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let source = temporary.path().join("source.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source_fixture()).unwrap()).unwrap();
    value["results"]["ballotItems"][0]["resourceTestPadding"] =
        serde_json::json!("x".repeat(8 * 1024 * 1024));
    std::fs::write(&source, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut child = collector()
        .arg("validate")
        .arg(&source)
        .arg("2026-11-03")
        .env("TMPDIR", &scratch)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_dir(&scratch).unwrap().next().is_some() {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "validation exited before cancellation test"
        );
        assert!(Instant::now() < deadline, "parser never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("validation did not join cancelled parser");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Collection interrupted")
    );
    assert!(
        std::fs::read_dir(&scratch).unwrap().next().is_none(),
        "task-owned spool remains after shutdown"
    );
}

#[test]
fn production_health_rejects_http_before_any_network_request() {
    let output = collector()
        .arg("health")
        .env("CIVPULSE_ENV", "prod")
        .env("SUPABASE_URL", "http://127.0.0.1:9")
        .env("SUPABASE_SERVICE_ROLE_KEY", "secret-not-for-logs")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("not allowed in this environment"));
    assert!(!error.contains("secret-not-for-logs"));
}
