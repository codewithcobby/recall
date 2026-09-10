//! Git metadata, from detection through to what ends up in the archive.
//!
//! Every test builds its own repository. None depends on the repository the
//! suite happens to run in, which would make the results depend on whoever
//! checked out the branch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use recall_store::Archive;

fn recall_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("run recall")
}

/// Run git for setup. Identity fixed so results do not depend on the machine.
fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=test@example.invalid"])
        .args(["-c", "user.name=Test"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A project that is a git repository with one commit.
fn repo_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical");
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    git(
        &root,
        &["commit", "--quiet", "--allow-empty", "-m", "first"],
    );
    (dir, root)
}

/// A settled Claude Code session for `project`, optionally claiming a branch.
fn claude_session(home: &Path, project: &Path, id: &str, branch: Option<&str>) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("mkdir");

    let branch_field = branch.map_or(String::new(), |b| format!("\"gitBranch\":\"{b}\","));
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"cwd\":\"{}\",{branch_field}\"message\":{{\"role\":\"user\",\"content\":\"work\"}}}}\n",
            project.display()
        ),
    )
    .expect("write");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .expect("backdate");
}

/// The single archived session's git context.
fn archived_git(root: &Path) -> recall_core::GitContext {
    Archive::open(root)
        .headers()
        .expect("headers")
        .into_iter()
        .flatten()
        .next()
        .expect("a session")
        .session
        .git
        .unwrap_or_default()
}

#[test]
fn the_repository_reaches_the_archive() {
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    claude_session(home.path(), &root, "s", Some("main"));

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let git_context = archived_git(&root);
    assert_eq!(
        git_context.repository.and_then(|p| p.canonicalize().ok()),
        Some(root.clone()),
        "the repository was not recorded"
    );
    assert_eq!(git_context.branch.as_deref(), Some("main"));
}

#[test]
fn the_branch_recorded_during_the_session_survives_a_later_checkout() {
    // The case this rule exists for. The session ran on one branch; by the time
    // it is archived the working tree has moved on. History must win.
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    claude_session(home.path(), &root, "s", Some("security/143-refresh-token"));

    // The working tree moves somewhere else entirely before the sync.
    git(&root, &["checkout", "--quiet", "-b", "something-else"]);

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    assert_eq!(
        archived_git(&root).branch.as_deref(),
        Some("security/143-refresh-token"),
        "detection overwrote the branch the session actually ran on"
    );
}

#[test]
fn detection_fills_a_branch_the_provider_did_not_record() {
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    git(&root, &["checkout", "--quiet", "-b", "detected-branch"]);
    claude_session(home.path(), &root, "s", None);

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    assert_eq!(
        archived_git(&root).branch.as_deref(),
        Some("detected-branch"),
        "nothing filled in the missing branch"
    );
}

#[test]
fn a_project_outside_a_repository_archives_cleanly() {
    // Plenty of work happens outside git. It must not stop a sync.
    let home = tempfile::tempdir().expect("home");
    let plain = tempfile::tempdir().expect("project");
    let root = plain.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "s", None);

    recall_in(&root, home.path(), &["init"]);
    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success(), "sync failed outside a repository");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 archived"),
        "the session was not archived"
    );

    let git_context = archived_git(&root);
    assert_eq!(git_context.repository, None);
    assert_eq!(git_context.branch, None);
}

#[test]
fn a_detached_head_records_no_branch() {
    // Matching #122: "HEAD" is not a branch name.
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    git(&root, &["checkout", "--quiet", "--detach"]);
    claude_session(home.path(), &root, "s", None);

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let git_context = archived_git(&root);
    assert_eq!(git_context.branch, None, "a detached HEAD became a branch");
    assert!(
        git_context.repository.is_some(),
        "the repository should still be known"
    );
}

#[test]
fn a_repository_with_no_commits_still_records_its_branch() {
    let home = tempfile::tempdir().expect("home");
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path().canonicalize().expect("canonical");
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    claude_session(home.path(), &root, "s", None);

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let git_context = archived_git(&root);
    assert_eq!(git_context.branch.as_deref(), Some("main"));
    assert_eq!(git_context.commit_at_start, None, "a commit was invented");
}

#[test]
fn commits_are_absent_in_every_case() {
    // Neither the provider nor detection can say which commit a session started
    // or ended on. Anything filled in here would be a guess.
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    claude_session(home.path(), &root, "s", Some("main"));

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let git_context = archived_git(&root);
    assert_eq!(git_context.commit_at_start, None);
    assert_eq!(git_context.commit_at_end, None);
}

#[test]
fn syncing_does_not_touch_the_repository() {
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    claude_session(home.path(), &root, "s", Some("main"));

    let head = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git");

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let after = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git");
    assert_eq!(head.stdout, after.stdout, "sync moved HEAD");

    // .recall/ is the only thing added, and it is not staged.
    let status = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["status", "--porcelain"])
        .output()
        .expect("git");
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.lines().all(|l| l.contains(".recall")),
        "sync changed something other than the archive: {status}"
    );
}

#[test]
fn the_branch_is_visible_in_recall_show() {
    let home = tempfile::tempdir().expect("home");
    let (_p, root) = repo_project();
    claude_session(home.path(), &root, "s", Some("fix/312-vendor-order"));

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let listed = recall_in(&root, home.path(), &["sessions"]);
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(listing.contains("fix/312-vendor-order"), "{listing}");

    let id = listing
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("an id")
        .to_string();
    let shown = recall_in(&root, home.path(), &["show", &id, "--summary"]);
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("branch   fix/312-vendor-order"), "{shown}");
    assert!(
        shown.contains("repo     "),
        "the repository was not shown: {shown}"
    );
}
