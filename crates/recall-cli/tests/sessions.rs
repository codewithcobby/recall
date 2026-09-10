//! `recall sessions`, now that it is answered from the index.
//!
//! The listing used to open every archive. These tests pin down where its
//! answer comes from, and what happens when the index cannot give one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
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
fn claude_session(home: &Path, project: &Path, id: &str, at: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");
    let path = dir.join(format!("{id}.jsonl"));
    let body = format!(
        "{{\"type\":\"user\",\"timestamp\":\"{at}\",\"cwd\":\"{p}\",\"gitBranch\":\"dev\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"cwd\":\"{p}\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}\n",
        p = project.display()
    );
    fs::write(&path, body).expect("write session");
    make_settled(&path);
}

/// A project with `count` archived sessions, already synced.
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
        );
    }
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());
    (home, project, root)
}

/// Every archive file in the project.
fn archives(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.join(".recall/sessions")];
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
    found.sort();
    found
}

#[test]
fn the_listing_reports_what_was_archived() {
    let (home, _project, root) = synced(3);
    let out = recall_in(&root, home.path(), &["sessions"]);

    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);
    assert!(text.contains("3 sessions"), "{text}");
    assert!(text.contains("claude-opus-5"), "{text}");
    assert!(text.contains("dev"), "{text}");
}

#[test]
fn the_answer_comes_from_the_index_not_the_archive_tree() {
    // Proved by taking the archives away. If the listing still answers, it was
    // never reading them — which is the whole point of #36.
    let (home, _project, root) = synced(2);
    for archive in archives(&root) {
        fs::remove_file(&archive).expect("remove archive");
    }

    let text = stdout(&recall_in(&root, home.path(), &["sessions"]));
    assert!(
        text.contains("2 sessions"),
        "the listing went back to the tree: {text}"
    );
}

#[test]
fn a_rebuilt_index_produces_the_same_listing() {
    // The property that makes the database disposable: the archives are enough
    // to reconstruct exactly what the index was answering with.
    let (home, _project, root) = synced(3);
    let before = stdout(&recall_in(&root, home.path(), &["sessions"]));

    let rebuilt = recall_in(&root, home.path(), &["sessions", "--rebuild"]);
    assert!(rebuilt.status.success(), "{rebuilt:?}");
    let after = stdout(&rebuilt);

    // The rebuild adds a line saying what it did; the table below it must match.
    let table = after
        .split_once("\n\n")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(after);
    assert_eq!(
        table, before,
        "the rebuilt index listed something different"
    );
}

#[test]
fn a_deleted_index_is_rebuilt_and_the_user_is_told() {
    // Silence here would look like a hang: this listing opens every archive.
    let (home, _project, root) = synced(3);
    fs::remove_file(root.join(".recall/index.db")).expect("delete index");

    let out = recall_in(&root, home.path(), &["sessions"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);

    assert!(text.contains("rebuilding"), "no explanation given: {text}");
    assert!(text.contains("3 sessions"), "{text}");
}

#[test]
fn an_index_that_is_not_a_database_is_rebuilt() {
    let (home, _project, root) = synced(2);
    fs::write(root.join(".recall/index.db"), b"not a database").expect("corrupt");

    let out = recall_in(&root, home.path(), &["sessions"]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("2 sessions"), "{}", stdout(&out));
}

#[test]
fn an_emptied_index_is_rebuilt_rather_than_reported_as_empty() {
    // What a sync that never reached the index leaves behind. Saying "no
    // sessions" while three sit in the archive would be a lie.
    let (home, _project, root) = synced(3);
    {
        let (mut index, _) =
            recall_index::Index::open(root.join(".recall/index.db")).expect("open");
        index.clear().expect("clear");
    }

    let text = stdout(&recall_in(&root, home.path(), &["sessions"]));
    assert!(text.contains("3 sessions"), "{text}");
}

#[test]
fn an_empty_archive_says_so_rather_than_rebuilding() {
    // Nothing to rebuild from, so the message is the useful one.
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    let text = stdout(&recall_in(&root, home.path(), &["sessions"]));
    assert!(text.contains("No sessions archived yet"), "{text}");
    assert!(!text.contains("rebuilding"), "rebuilt nothing: {text}");
}

#[test]
fn the_fast_path_does_not_rebuild() {
    // A listing straight after a sync has a current index and must not touch
    // the archives. If this starts announcing a rebuild, the staleness check
    // has become something that fires on every run.
    let (home, _project, root) = synced(3);
    let text = stdout(&recall_in(&root, home.path(), &["sessions"]));
    assert!(
        !text.contains("rebuilding"),
        "rebuilt unnecessarily: {text}"
    );
}

#[test]
fn a_damaged_archive_is_reported_during_a_rebuild() {
    // A rebuild is the one moment every archive is opened, so it is the moment
    // to say one cannot be read. Indexing four of five silently would make the
    // fifth look like it never existed.
    let (home, _project, root) = synced(3);
    let victim = archives(&root).remove(1);
    fs::write(&victim, b"no longer an archive").expect("damage");

    let out = recall_in(&root, home.path(), &["sessions", "--rebuild"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);

    assert!(text.contains("could not be read"), "{text}");
    assert!(text.contains("2 sessions"), "{text}");
}

#[test]
fn listing_without_init_says_what_to_do() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let out = recall_in(project.path(), home.path(), &["sessions"]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("recall init"), "{stderr}");
}
