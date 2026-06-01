//! Black-box tests for the `pax` binary, driving the deterministic mock backend
//! so they need no hardware. They run the compiled binary via `CARGO_BIN_EXE_pax`
//! (set by Cargo for integration tests), so there are no extra dependencies.

use std::process::Command;

fn pax() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_pax"));
    c.args(["--backend", "mock"]);
    c
}

#[test]
fn spoof_subcommand_sets_the_address() {
    let out = pax()
        .args(["spoof", "02:00:00:11:22:33"])
        .output()
        .expect("run pax");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("02:00:00:11:22:33"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn global_spoof_flag_runs_the_command_under_it() {
    // `--spoof X scan` should set the address, then scan successfully.
    let out = pax()
        .args(["--spoof", "02:00:00:AA:BB:CC", "scan"])
        .output()
        .expect("run pax");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("device(s) in range"), "got: {stdout}");
}

#[test]
fn invalid_spoof_address_is_rejected() {
    let out = pax()
        .args(["spoof", "not-an-address"])
        .output()
        .expect("run pax");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid"), "stderr: {stderr}");
}

/// The mock backend advertises spoofing support, so `--spoof` must NOT be
/// rejected as unsupported (the capability gate only blocks backends that can't).
#[test]
fn mock_backend_accepts_spoofing() {
    let out = pax()
        .args(["--spoof", "02:00:00:00:00:01", "doctor"])
        .output()
        .expect("run pax");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}
