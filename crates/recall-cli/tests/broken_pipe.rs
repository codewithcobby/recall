//! What happens when the thing reading Recall's output stops reading.
//!
//! `recall sessions | head -1` is ordinary shell usage. It used to panic (#149).
//!
//! Size is the whole difficulty in reproducing this. A pipe holds about 64 KB,
//! so a short listing is written in full before `head` has even exited and
//! nothing ever fails. These tests therefore build an archive large enough that
//! the writer is guaranteed to still be going when the reader leaves.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use recall_core::{Provider, Session, SessionHeader};
use recall_index::{Index, IndexedSession};

/// Comfortably more than a pipe buffer holds: each row is ~90 bytes, so this is
/// roughly 270 KB of output.
const MANY: usize = 3000;

fn recall_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("failed to run recall")
}

/// A project whose index holds `MANY` sessions.
///
/// The rows are written straight to the index rather than archived through
/// sync: this test is about output, and archiving three thousand sessions to
/// produce three thousand lines would make it slow for no extra coverage.
fn project_with_a_long_listing() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    let (mut index, _) = Index::open(root.join(".recall/index.db")).expect("open index");
    let provider = Provider::new("claude-code").expect("provider");
    let start = time::macros::datetime!(2026-09-08 12:00:00 UTC);

    let rows: Vec<IndexedSession> = (0..MANY)
        .map(|n| {
            let mut session = Session::new(
                provider.clone(),
                format!("session-{n}"),
                start + time::Duration::seconds(n as i64),
            );
            session.model = Some("claude-opus-5".into());
            IndexedSession::new(
                SessionHeader {
                    session,
                    event_count: Some(n),
                },
                format!("sessions/2026/09/08/{n}.zst"),
            )
        })
        .collect();
    index.upsert(&rows).expect("upsert");
    drop(index);

    (home, project, root)
}

/// Run `recall`, read one line, then close the pipe and see how it took it.
fn read_one_line_then_walk_away(root: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn recall");

    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).expect("read one line");

    // What `head` does when it has what it came for.
    drop(reader);

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    let status = child.wait().expect("wait");
    (status.success(), stderr)
}

#[test]
fn listing_into_a_reader_that_stops_early_is_not_a_failure() {
    let (home, _project, root) = project_with_a_long_listing();
    let (ok, stderr) = read_one_line_then_walk_away(&root, home.path(), &["sessions"]);

    assert!(
        !stderr.contains("panicked"),
        "recall panicked when the reader left:\n{stderr}"
    );
    assert!(stderr.is_empty(), "expected no complaint, got:\n{stderr}");
    assert!(ok, "expected a successful exit");
}

#[test]
fn a_rebuild_into_a_reader_that_stops_early_is_not_a_failure() {
    // The rebuild path prints a line, then does real work before printing the
    // table — which is exactly the gap that lets the reader leave first. This
    // is how #149 was found.
    let (home, _project, root) = project_with_a_long_listing();
    let (ok, stderr) = read_one_line_then_walk_away(&root, home.path(), &["sessions", "--rebuild"]);

    assert!(
        !stderr.contains("panicked"),
        "recall panicked during a rebuild:\n{stderr}"
    );
    assert!(ok, "expected a successful exit, stderr was:\n{stderr}");
}

#[test]
fn showing_a_session_into_a_reader_that_stops_early_is_not_a_failure() {
    // `recall show` writes through `?`, so this used to surface as
    // "error: Broken pipe (os error 32)" and a non-zero exit rather than a
    // panic. Same cause, different path out.
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    // A transcript long enough to outlast the reader.
    let slug = root.display().to_string().replace('/', "-");
    let dir = home.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&dir).expect("create project directory");
    let mut body = String::new();
    for n in 0..4000 {
        body.push_str(&format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":\"line {n} of a long conversation\"}}}}\n",
            root.display()
        ));
    }
    let path = dir.join("long.jsonl");
    std::fs::write(&path, body).expect("write session");
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(long_ago)
        .expect("backdate");

    assert!(recall_in(&root, home.path(), &["sync"]).status.success());
    let listing = recall_in(&root, home.path(), &["sessions"]);
    let text = String::from_utf8_lossy(&listing.stdout);
    let id = text
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("a session id")
        .to_string();

    let (ok, stderr) = read_one_line_then_walk_away(&root, home.path(), &["show", &id]);

    assert!(
        !stderr.contains("Broken pipe"),
        "recall reported a broken pipe as an error:\n{stderr}"
    );
    assert!(!stderr.contains("panicked"), "recall panicked:\n{stderr}");
    assert!(ok, "expected a successful exit, stderr was:\n{stderr}");
}

#[test]
fn a_reader_that_stays_still_gets_everything() {
    // The other half: nothing above may cut output short for a reader that is
    // actually listening.
    let (home, _project, root) = project_with_a_long_listing();
    let out = recall_in(&root, home.path(), &["sessions"]);

    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("{MANY} sessions")),
        "the listing was truncated: {} lines",
        text.lines().count()
    );
    // Header, MANY rows, a blank line and the count.
    assert!(
        text.lines().count() >= MANY + 2,
        "expected at least {} lines, got {}",
        MANY + 2,
        text.lines().count()
    );
}
