//! Surface-level checks on the binary: the flags work, commands that are not
//! implemented say so, and `recall init` does what it claims in a real
//! directory.

use std::path::Path;
use std::process::{Command, Output};

fn recall(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .output()
        .expect("failed to run the recall binary")
}

/// Run in a specific directory, so no test depends on where the suite started.
fn recall_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
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
    // Exiting 0 would tell a script the work was done. `search` is #38.
    let out = recall(&["search", "anything"]);
    assert!(!out.status.success(), "`recall search` exited zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not implemented yet"),
        "`recall search` said: {stderr}"
    );
}

#[test]
fn sessions_without_init_refuses_and_says_what_to_do() {
    let project = tempfile::tempdir().expect("temp dir");
    let out = recall_in(project.path(), &["sessions"]);

    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("recall init"),
        "the error did not say what to do"
    );
}

#[test]
fn sessions_on_an_empty_archive_says_so() {
    let project = tempfile::tempdir().expect("temp dir");
    assert!(recall_in(project.path(), &["init"]).status.success());

    let out = recall_in(project.path(), &["sessions"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No sessions archived yet") && stdout.contains("recall sync"),
        "said: {stdout}"
    );
}

#[test]
fn sync_without_init_refuses_and_says_what_to_do() {
    let project = tempfile::tempdir().expect("temp dir");
    let out = recall_in(project.path(), &["sync"]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("recall init"),
        "the error did not say what to do: {stderr}"
    );
}

#[test]
fn sync_on_an_initialized_project_with_no_sessions_succeeds_quietly() {
    // No provider sessions belong to a throwaway directory, so this is the
    // ordinary "nothing to do" case rather than an error.
    let project = tempfile::tempdir().expect("temp dir");
    assert!(recall_in(project.path(), &["init"]).status.success());

    let out = recall_in(project.path(), &["sync"]);
    assert!(
        out.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("No AI sessions found"), "said: {stdout}");
}

#[test]
fn init_creates_the_archive_in_the_working_directory() {
    let project = tempfile::tempdir().expect("temp dir");
    let out = recall_in(project.path(), &["init"]);

    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(project.path().join(".recall/sessions").is_dir());
    assert!(project.path().join(".recall/tmp").is_dir());
    assert!(project.path().join(".recall/config.toml").is_file());
}

#[test]
fn init_run_twice_succeeds_and_says_so() {
    let project = tempfile::tempdir().expect("temp dir");
    assert!(recall_in(project.path(), &["init"]).status.success());

    let second = recall_in(project.path(), &["init"]);
    assert!(second.status.success(), "the second init must not fail");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("already initialized"),
        "second init said: {stdout}"
    );
}

#[test]
fn init_suggests_gitignore_when_the_project_has_one_missing_the_entry() {
    let project = tempfile::tempdir().expect("temp dir");
    std::fs::write(project.path().join(".gitignore"), "target/\n").expect("write");

    let out = recall_in(project.path(), &["init"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(".gitignore"), "no hint given: {stdout}");
}

#[test]
fn init_does_not_suggest_gitignore_when_the_entry_is_already_there() {
    // A fresh project, so the run does create something — this has to fail for
    // the right reason, not because nothing happened.
    let project = tempfile::tempdir().expect("temp dir");
    std::fs::write(project.path().join(".gitignore"), "target/\n.recall/\n").expect("write");

    let out = recall_in(project.path(), &["init"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Initialized Recall"),
        "init did nothing: {stdout}"
    );
    assert!(
        !stdout.contains("does not mention"),
        "hint given despite the entry being present: {stdout}"
    );
}

#[test]
fn the_gitignore_hint_is_not_repeated_on_later_runs() {
    // It is a suggestion, not a nag. Once made, a run that changes nothing
    // should stay quiet.
    let project = tempfile::tempdir().expect("temp dir");
    std::fs::write(project.path().join(".gitignore"), "target/\n").expect("write");

    let first = recall_in(project.path(), &["init"]);
    assert!(String::from_utf8_lossy(&first.stdout).contains("does not mention"));

    let second = recall_in(project.path(), &["init"]);
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        !stdout.contains("does not mention"),
        "hint repeated on a run that changed nothing: {stdout}"
    );
}

#[test]
fn finishing_an_interrupted_init_does_not_claim_to_be_a_fresh_one() {
    // .recall/ already exists, so this run completes an earlier one. Saying
    // "Initialized" here would misdescribe what happened.
    let project = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(project.path().join(".recall")).expect("create .recall");

    let out = recall_in(project.path(), &["init"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.contains("Completed Recall setup"),
        "expected the completion wording, got: {stdout}"
    );
    assert!(!stdout.contains("Initialized Recall"), "{stdout}");
}

#[test]
fn init_never_edits_the_users_gitignore() {
    let project = tempfile::tempdir().expect("temp dir");
    let gitignore = project.path().join(".gitignore");
    std::fs::write(&gitignore, "target/\n").expect("write");

    recall_in(project.path(), &["init"]);

    assert_eq!(
        std::fs::read_to_string(&gitignore).expect("read"),
        "target/\n",
        "recall init modified the user's .gitignore"
    );
}
