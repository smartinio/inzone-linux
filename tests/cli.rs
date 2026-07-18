use std::process::Command;

#[test]
fn binary_entrypoint_covers_success_and_failure() {
    let binary = env!("CARGO_BIN_EXE_inzone-buds");

    let help = Command::new(binary).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout).unwrap().contains("Usage:"));

    let error = Command::new(binary).arg("--invalid").output().unwrap();
    assert!(!error.status.success());
    assert!(
        String::from_utf8(error.stderr)
            .unwrap()
            .contains("unknown option")
    );

    let missing_device = Command::new(binary)
        .args(["--device", "/tmp/inzone-buds-coverage-no-device"])
        .output()
        .unwrap();
    assert!(!missing_device.status.success());
    assert!(
        String::from_utf8(missing_device.stderr)
            .unwrap()
            .contains("unverified device")
    );
}
