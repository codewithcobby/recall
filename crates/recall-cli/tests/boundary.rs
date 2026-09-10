//! The line between the index and the archive.
//!
//! `docs/index-and-archive.md` states the rules; this file is what stops them
//! becoming aspirational. Each test here corresponds to a numbered rule, and a
//! change that crosses the boundary should fail one of them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use recall_index::{Index, IndexedSession};

fn recall_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("failed to run recall")
}

fn make_settled(path: &Path) {
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60);
    let f = fs::File::options()
        .write(true)
        .open(path)
        .expect("open for touch");
    f.set_modified(long_ago).expect("backdate");
}

/// A session whose transcript carries text we can look for afterwards.
fn claude_session(home: &Path, project: &Path, id: &str, at: &str, text: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");
    let path = dir.join(format!("{id}.jsonl"));
    let body = format!(
        "{{\"type\":\"user\",\"timestamp\":\"{at}\",\"cwd\":\"{p}\",\"gitBranch\":\"dev\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"cwd\":\"{p}\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n",
        p = project.display()
    );
    fs::write(&path, body).expect("write session");
    make_settled(&path);
}

fn synced(count: usize) -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    assert!(recall_in(&root, home.path(), &["init"]).status.success());
    for n in 0..count {
        claude_session(
            home.path(),
            &root,
            &format!("session-{n}"),
            &format!("2026-09-0{}T12:00:00.000Z", n + 1),
            "hello",
        );
    }
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());
    (home, project, root)
}

fn rows(root: &Path) -> Vec<IndexedSession> {
    let (index, _) = Index::open(root.join(".recall/index.db")).expect("open index");
    index.list().expect("list")
}

/// Rule 2: the index is rebuildable from the archives alone.
#[test]
fn a_rebuilt_index_lists_exactly_what_the_archives_hold() {
    let (home, _project, root) = synced(4);
    let before = rows(&root);
    assert_eq!(before.len(), 4);

    assert!(recall_in(&root, home.path(), &["sessions", "--rebuild"])
        .status
        .success());

    // Compared as rows, not as printed text: a column the listing does not show
    // could still be lost, and it would not be noticed by reading the table.
    assert_eq!(rows(&root), before);
}

/// Rule 3: deleting `index.db` loses nothing.
#[test]
fn a_deleted_index_costs_nothing_but_time() {
    let (home, _project, root) = synced(4);
    let before = rows(&root);

    fs::remove_file(root.join(".recall/index.db")).expect("delete index");
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());

    assert_eq!(
        rows(&root),
        before,
        "the archives did not reproduce the index"
    );
}

/// Rule 1, end to end: no conversation content reaches the database.
#[test]
fn no_part_of_a_transcript_reaches_the_database() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    let secret = "hunter2-quinoa-tessellation";
    claude_session(
        home.path(),
        &root,
        "abc",
        "2026-09-08T12:00:00.000Z",
        secret,
    );
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());

    // The bytes on disk, not the API. Asking the index whether it stored the
    // text would answer with the same assumptions that put it there.
    let bytes = fs::read(root.join(".recall/index.db")).expect("read index");
    assert!(
        !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
        "the transcript reached the database"
    );

    // And the archive does have it, so the test is looking for something that
    // is genuinely present somewhere.
    let found = walk(&root.join(".recall/sessions")).into_iter().any(|p| {
        let raw = zstd::decode_all(fs::File::open(&p).expect("open")).unwrap_or_default();
        raw.windows(secret.len()).any(|w| w == secret.as_bytes())
    });
    assert!(
        found,
        "the fixture never archived the text being looked for"
    );
}

/// Rule 1, again: the count of events is metadata, the events are not.
#[test]
fn the_index_records_how_many_events_there_were_not_what_they_said() {
    let (_home, _project, root) = synced(1);
    let row = rows(&root).remove(0);

    assert_eq!(
        row.event_count,
        Some(2),
        "the count is metadata and is kept"
    );
    assert!(
        row.session.events.is_empty(),
        "the index handed back a transcript"
    );
}

/// Rule 4: the archive wins.
#[test]
fn a_corrupt_index_does_not_cost_the_user_a_session() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    fs::write(root.join(".recall/index.db"), b"not a database").expect("corrupt");
    claude_session(home.path(), &root, "abc", "2026-09-08T12:00:00.000Z", "hi");

    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success(), "sync failed: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 archived"),
        "the session was not archived"
    );
}

/// Rule 5: an index this build cannot understand is replaced, not refused.
#[test]
fn an_index_from_a_newer_build_is_replaced_rather_than_refused() {
    let (home, _project, root) = synced(2);
    let before = rows(&root);

    {
        // Reach past the API to fake a future schema.
        let (index, _) = Index::open(root.join(".recall/index.db")).expect("open");
        drop(index);
        let connection =
            rusqlite::Connection::open(root.join(".recall/index.db")).expect("sqlite open");
        connection
            .execute_batch("PRAGMA user_version = 9999")
            .expect("bump");
    }

    let out = recall_in(&root, home.path(), &["sessions"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(rows(&root), before, "the rebuild lost something");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                found.push(p);
            }
        }
    }
    found
}
