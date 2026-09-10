//! `recall watch` end to end, against a fake home directory.
//!
//! These exist to pin one property: watch archives through **the same path**
//! `recall sync` does. A watcher with an ingestion route of its own would drift,
//! and only one of the two would get fixed when something broke.
//!
//! The command runs until interrupted, so each test starts it as a child
//! process, reads until it has said what it did, and stops it.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long a test waits for the command to say something before giving up.
const DEADLINE: Duration = Duration::from_secs(30);

/// Put a Claude Code session where the adapter will find it.
///
/// Backdated an hour, because `sync` deliberately leaves a file alone until it
/// has been quiet for five minutes — a session still being written must never
/// be archived half-finished.
fn claude_session(home: &Path, project: &Path, id: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("mkdir");

    let path = dir.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":\"work\"}}}}\n",
            project.display()
        ),
    )
    .expect("write session");

    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .expect("backdate")
}

fn recall(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("run recall")
}

/// Start `recall watch`, collect what it says until it settles, then stop it.
///
/// Returns everything it printed before being stopped. Reading happens on
/// another thread so a command that says nothing cannot hang the test.
fn watch_until_quiet(dir: &Path, home: &Path) -> String {
    let mut child: Child = Command::new(env!("CARGO_BIN_EXE_recall"))
        .arg("watch")
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn recall watch");

    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // A send failure means the test stopped reading. Nothing to do.
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // The startup pass and the "Press Ctrl-C" line come out immediately.
    // Anything after them arrives only if something changed, so once the
    // output goes quiet there is nothing further to wait for.
    let mut said = Vec::new();
    while let Ok(line) = rx.recv_timeout(if said.is_empty() {
        DEADLINE
    } else {
        Duration::from_secs(2)
    }) {
        let done = line.contains("Press Ctrl-C");
        said.push(line);
        if done {
            break;
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(rx);
    let _ = reader.join();

    said.join("\n")
}

/// A project with Recall initialized and a fake home beside it.
fn project() -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("home");
    let dir = tempfile::tempdir().expect("project");
    (home, dir)
}

#[test]
fn watch_archives_a_session_through_the_same_path_as_sync() {
    // The property this whole issue exists for. If watch had its own ingestion
    // route, this would still pass while the two slowly diverged — so the next
    // test checks that sync recognises what watch archived.
    let (home, dir) = project();
    let root = dir.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "session-one");

    recall(&root, home.path(), &["init"]);
    let said = watch_until_quiet(&root, home.path());

    assert!(
        said.contains("1 archived"),
        "watch did not archive the session: {said}"
    );
}

#[test]
fn sync_recognises_what_watch_archived() {
    // Deduplication is #25's, and it works off the derived session id. If watch
    // wrote through a different path, or wrote a differently-derived id, sync
    // would archive the same conversation a second time.
    let (home, dir) = project();
    let root = dir.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "session-one");

    recall(&root, home.path(), &["init"]);
    watch_until_quiet(&root, home.path());

    let out = recall(&root, home.path(), &["sync"]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("0 archived, 1 already had"),
        "sync did not recognise the session watch archived: {said}"
    );
}

#[test]
fn watch_recognises_what_sync_archived() {
    // And the same in the other direction.
    let (home, dir) = project();
    let root = dir.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "session-one");

    recall(&root, home.path(), &["init"]);
    recall(&root, home.path(), &["sync"]);

    let said = watch_until_quiet(&root, home.path());
    assert!(
        said.contains("0 archived, 1 already had"),
        "watch re-archived a session sync already had: {said}"
    );
}

#[test]
fn the_archive_watch_writes_is_readable_by_every_other_command() {
    // Watch is not a side door into the archive: what it writes is an ordinary
    // session, indexed and listable like any other.
    let (home, dir) = project();
    let root = dir.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "session-one");

    recall(&root, home.path(), &["init"]);
    watch_until_quiet(&root, home.path());

    let listed = recall(&root, home.path(), &["sessions"]);
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.contains("claude-code"),
        "the session watch archived was not listed: {listed}"
    );

    let verified = recall(&root, home.path(), &["verify"]);
    let verified = String::from_utf8_lossy(&verified.stdout);
    assert!(
        verified.contains("1 readable, 0 damaged"),
        "the archive watch wrote did not verify: {verified}"
    );
}

#[test]
fn watch_refuses_an_uninitialized_project_the_way_sync_does() {
    // Shared path, shared refusal. Watch must not create an archive as a side
    // effect of being asked to watch.
    let (home, dir) = project();
    let root = dir.path().canonicalize().expect("canonical");

    let said = watch_until_quiet(&root, home.path());
    assert!(
        !said.contains("Watching"),
        "watch started without an initialized project: {said}"
    );
    assert!(
        !root.join(".recall").exists(),
        "watch created an archive in an uninitialized project"
    );
}

#[test]
fn watch_says_which_directories_it_is_watching() {
    // `recall sync` prints where it looked when it finds nothing; watch has to
    // do the same, or a user cannot tell "no sessions yet" from "watching the
    // wrong place".
    let (home, dir) = project();
    let root = dir.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "session-one");

    recall(&root, home.path(), &["init"]);
    let said = watch_until_quiet(&root, home.path());

    assert!(
        said.contains("Watching") && said.contains(".claude"),
        "watch did not say where it was looking: {said}"
    );
}
