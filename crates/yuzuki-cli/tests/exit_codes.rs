//! The yuzuki binary's process exit codes. Kept apart from tests/golden.rs:
//! spawning children next to an in-process endpoint resets its sockets on
//! Windows.

use std::process::Command;

#[test]
fn the_binary_returns_the_contract_exit_codes() {
    let missing = std::env::temp_dir()
        .join("yuzuki-cli-no-such-dir")
        .join("control.json");
    let status = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_yuzuki"))
            .args(["--json", "--control-file"])
            .arg(&missing)
            .args(args)
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(status(&["job", "list"]), Some(3));
    assert_eq!(status(&["job", "cancel"]), Some(2));
    assert_eq!(status(&["--help"]), Some(0));
}
