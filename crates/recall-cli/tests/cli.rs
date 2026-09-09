//! Surface-level checks on the binary: the flags work, and a command that is
//! not implemented yet says so instead of exiting successfully.

use std::process::{Command, Output};

fn recall(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .output()
        .expect("failed to run the recall binary")
}

#[test]
fn version_reports_the_crate_version() {
    let out = recall(&["--version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version printed {stdout:?}"
    );
}

#[test]
fn help_lists_every_command() {
    let out = recall(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for command in ["init", "sync", "sessions", "show", "search"] {
        assert!(
            stdout.contains(command),
            "--help omitted {command}: {stdout}"
        );
    }
}

#[test]
fn no_arguments_is_an_error_not_a_silent_success() {
    let out = recall(&[]);
    assert!(!out.status.success());
}

#[test]
fn unknown_command_is_rejected() {
    let out = recall(&["definitely-not-a-command"]);
    assert!(!out.status.success());
}

#[test]
fn unimplemented_commands_do_not_exit_zero() {
    // Until these are implemented, exiting 0 would tell a script the work was
    // done. Each names the issue that will implement it.
    for command in ["init", "sync", "sessions"] {
        let out = recall(&[command]);
        assert!(!out.status.success(), "`recall {command}` exited zero");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("not implemented yet"),
            "`recall {command}` said: {stderr}"
        );
    }
}
