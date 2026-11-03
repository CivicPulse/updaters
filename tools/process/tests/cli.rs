use std::process::Command;

#[test]
fn scaffold_process_contract() {
    let binary = env!("CARGO_BIN_EXE_civpulse-process");
    let check = Command::new(binary)
        .arg("--check")
        .env("RUST_LOG", "info")
        .output()
        .expect("start check command");
    assert!(check.status.success());
    assert!(String::from_utf8_lossy(&check.stderr).contains("scaffold check passed"));

    for args in [vec![], vec!["--unknown"]] {
        let result = Command::new(binary)
            .args(args)
            .output()
            .expect("start command");
        assert!(!result.status.success());
    }

    let invalid_log = Command::new(binary)
        .arg("--check")
        .env("RUST_LOG", "[")
        .output()
        .expect("start invalid configuration command");
    assert!(!invalid_log.status.success());
}
