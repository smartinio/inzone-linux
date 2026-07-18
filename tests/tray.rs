#![cfg(feature = "tray")]

use std::process::Command;

#[test]
fn tray_entrypoint_starts_refreshes_and_exits_cleanly() {
    let output = Command::new("timeout")
        .args(["--signal=KILL", "10s", "dbus-run-session", "--"])
        .arg(env!("CARGO_BIN_EXE_inzone-buds-tray"))
        .env(
            "INZONE_BUDS_TRAY_TEST_DEVICE",
            "/tmp/inzone-buds-coverage-no-device",
        )
        .env("INZONE_BUDS_TRAY_TEST_EXIT_AFTER_REFRESH", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
}
