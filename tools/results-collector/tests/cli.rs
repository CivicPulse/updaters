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
