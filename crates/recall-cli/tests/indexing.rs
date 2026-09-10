//! The index, as `recall sync` fills it.
//!
//! These tests go through the binary and then look at `.recall/index.db`
//! directly. Reading it through the same code that wrote it would agree with
//! itself no matter what landed on disk.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use recall_index::Index;

/// Run recall in `dir`, with Claude Code's home pointed somewhere harmless.
fn recall_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("failed to run recall")
}

/// Backdate a file so sync treats it as settled rather than in progress.
fn make_settled(path: &Path) {
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60);
    let f = fs::File::options()
        .write(true)
        .open(path)
        .expect("open for touch");
    f.set_modified(long_ago).expect("backdate");
}

/// A Claude Code session file for `project`, with `id`.
fn claude_session(home: &Path, project: &Path, id: &str, at: &str, text: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");
    let path = dir.join(format!("{id}.jsonl"));
    let body = format!(
        "{{\"type\":\"user\",\"timestamp\":\"{at}\",\"cwd\":\"{p}\",\"gitBranch\":\"dev\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"cwd\":\"{p}\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}\n",
        p = project.display()
    );
    fs::write(&path, body).expect("write session");
    make_settled(&path);
}

/// A project with `count` archived sessions, already synced.
fn synced(count: usize) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    assert!(recall_in(&root, home.path(), &["init"]).status.success());
    for n in 0..count {
        claude_session(
            home.path(),
            &root,
            &format!("session-{n}"),
            // Distinct, ascending start times so ordering is observable.
            &format!("2026-09-0{}T12:00:00.000Z", n + 1),
            "hello",
        );
    }
    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success(), "sync failed: {out:?}");

    (home, project, root)
}

/// Open the index the way an outside reader would.
fn open(root: &Path) -> Index {
    let (index, _) = Index::open(root.join(".recall/index.db")).expect("open index");
    index
}

#[test]
fn the_index_is_created_by_sync() {
    let (_home, _project, root) = synced(1);
    assert!(
        root.join(".recall/index.db").is_file(),
        "sync did not create the index"
    );
}

#[test]
fn archived_sessions_appear_in_the_index() {
    let (_home, _project, root) = synced(3);
    assert_eq!(open(&root).count().expect("count"), 3);
}

#[test]
fn the_index_agrees_with_the_archive() {
    // The row has to point at a file that is really there, or the listing
    // built on it in #36 will offer sessions that cannot be opened.
    let (_home, _project, root) = synced(2);

    for indexed in open(&root).list().expect("list") {
        let archive = root.join(".recall").join(&indexed.archive_path);
        assert!(
            archive.is_file(),
            "{} does not exist",
            indexed.archive_path.display()
        );
        assert!(
            indexed.archive_path.is_relative(),
            "{} is absolute",
            indexed.archive_path.display()
        );
    }
}

#[test]
fn the_metadata_survives_the_trip_through_sync() {
    let (_home, _project, root) = synced(1);
    let indexed = open(&root).list().expect("list").remove(0);

    assert_eq!(indexed.session.provider.as_str(), "claude-code");
    assert_eq!(indexed.session.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(
        indexed
            .session
            .git
            .as_ref()
            .and_then(|g| g.branch.as_deref()),
        Some("dev")
    );
    assert_eq!(indexed.event_count, Some(2));
}

#[test]
fn sessions_are_indexed_newest_first() {
    let (_home, _project, root) = synced(3);
    let listed = open(&root).list().expect("list");

    let starts: Vec<_> = listed.iter().map(|s| s.session.started_at).collect();
    let mut expected = starts.clone();
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(starts, expected, "the index did not order by start time");
}

#[test]
fn syncing_again_does_not_duplicate_rows() {
    // Sync is expected to run repeatedly over the same sessions.
    let (home, _project, root) = synced(2);
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());
    assert_eq!(open(&root).count().expect("count"), 2);
}

#[test]
fn a_deleted_index_is_rebuilt_by_the_next_sync() {
    // The reconciliation path, and the reason a lost index is an inconvenience
    // rather than data loss.
    let (home, _project, root) = synced(3);
    fs::remove_file(root.join(".recall/index.db")).expect("delete index");

    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success(), "sync failed: {out:?}");

    assert_eq!(
        open(&root).count().expect("count"),
        3,
        "the index did not catch up with the archive"
    );
}

#[test]
fn an_index_that_fell_behind_is_caught_up() {
    // Not the same as deleting it: here the database is fine and simply missing
    // rows, which is what a failed write on an earlier run leaves behind.
    let (home, _project, root) = synced(3);
    {
        let mut index = open(&root);
        index.clear().expect("clear");
    }
    assert_eq!(open(&root).count().expect("count"), 0);

    assert!(recall_in(&root, home.path(), &["sync"]).status.success());
    assert_eq!(open(&root).count().expect("count"), 3);
}

#[test]
fn a_corrupt_index_does_not_cost_the_user_a_session() {
    // The rule the whole design turns on: the archive wins. A database full of
    // rubbish must not stop sync from preserving a conversation.
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    assert!(recall_in(&root, home.path(), &["init"]).status.success());
    fs::write(root.join(".recall/index.db"), b"not a database").expect("corrupt");
    claude_session(home.path(), &root, "abc", "2026-09-08T12:00:00.000Z", "hi");

    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success(), "sync failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 archived"), "not archived: {stdout}");

    // And the index recovered rather than staying broken.
    assert_eq!(open(&root).count().expect("count"), 1);
}

#[test]
fn the_index_never_holds_conversation_content() {
    // The boundary #37 documents. Checked against the bytes on disk, because
    // that is where it would leak.
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    assert!(recall_in(&root, home.path(), &["init"]).status.success());
    let secret = "correct-horse-battery-staple";
    claude_session(
        home.path(),
        &root,
        "abc",
        "2026-09-08T12:00:00.000Z",
        secret,
    );
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());

    let bytes = fs::read(root.join(".recall/index.db")).expect("read index");
    assert!(
        !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
        "the transcript reached the database"
    );
}

#[cfg(unix)]
#[test]
fn the_index_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let (_home, _project, root) = synced(1);
    let mode = fs::metadata(root.join(".recall/index.db"))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "index.db is {mode:o}, expected 600");
}
