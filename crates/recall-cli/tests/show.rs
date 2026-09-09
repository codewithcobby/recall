//! `recall sessions` and `recall show`, against archives built by sync.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn recall_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("run recall")
}

/// A settled Claude Code session for `project`.
fn claude_session(home: &Path, project: &Path, id: &str, at: &str, text: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"{at}\",\"cwd\":\"{}\",\"gitBranch\":\"dev\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n\
             {{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"cwd\":\"{}\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read_file\",\"input\":{{\"path\":\"a.rs\"}}}}]}}}}\n",
            project.display(),
            project.display()
        ),
    )
    .expect("write session");
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(long_ago)
        .expect("backdate");
}

/// A project with sessions already synced.
fn synced(count: usize) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    for i in 0..count {
        claude_session(
            home.path(),
            &root,
            &format!("session-{i}"),
            &format!("2026-09-0{}T12:00:00.000Z", i + 1),
            &format!("message in session {i}"),
        );
    }
    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);
    (home, project, root)
}

#[test]
fn sessions_lists_what_was_archived() {
    let (home, _p, root) = synced(3);
    let out = recall_in(&root, home.path(), &["sessions"]);
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ID"), "no header row: {stdout}");
    assert!(stdout.contains("claude-opus-5"), "no model: {stdout}");
    assert!(stdout.contains("dev"), "no branch: {stdout}");
    assert!(stdout.contains("3 sessions"), "no count: {stdout}");
}

#[test]
fn sessions_reports_how_many_events_each_holds() {
    let (home, _p, root) = synced(1);
    let out = recall_in(&root, home.path(), &["sessions"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Two records produce a message and a tool call.
    assert!(
        stdout.contains(" 2 "),
        "the event count was not reported: {stdout}"
    );
}

#[test]
fn sessions_are_listed_newest_first() {
    let (home, _p, root) = synced(3);
    let out = recall_in(&root, home.path(), &["sessions"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let dates: Vec<&str> = stdout
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|d| d.starts_with("2026"))
        .collect();
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted, "not newest first: {dates:?}");
}

#[test]
fn show_prints_the_conversation() {
    let (home, _p, root) = synced(1);
    let listed = recall_in(&root, home.path(), &["sessions"]);
    let id = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("an id")
        .to_string();

    let out = recall_in(&root, home.path(), &["show", &id]);
    assert!(
        out.status.success(),
        "show failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(stdout.contains("message in session 0"), "{stdout}");
    assert!(stdout.contains("you"), "the speaker was not labelled");
    assert!(stdout.contains("read_file"), "the tool call was not shown");
    assert!(stdout.contains("branch   dev"), "{stdout}");
}

#[test]
fn show_accepts_a_short_prefix() {
    let (home, _p, root) = synced(1);
    let listed = recall_in(&root, home.path(), &["sessions"]);
    let id = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("an id")
        .to_string();

    // Four characters, well short of the 32 an id has.
    let out = recall_in(&root, home.path(), &["show", &id[..4]]);
    assert!(out.status.success(), "a short prefix was rejected");
}

#[test]
fn show_summary_omits_the_transcript() {
    let (home, _p, root) = synced(1);
    let listed = recall_in(&root, home.path(), &["sessions"]);
    let id = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("an id")
        .to_string();

    let out = recall_in(&root, home.path(), &["show", &id, "--summary"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("events   2"), "{stdout}");
    assert!(
        !stdout.contains("message in session 0"),
        "the transcript was printed anyway: {stdout}"
    );
}

#[test]
fn an_ambiguous_prefix_asks_rather_than_guesses() {
    // Showing one of several would be showing the wrong conversation some of
    // the time.
    let (home, _p, root) = synced(3);
    let out = recall_in(&root, home.path(), &["show", ""]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("matches 3 sessions"), "said: {stderr}");
    assert!(stderr.contains("use more characters"), "said: {stderr}");
}

#[test]
fn an_unknown_prefix_says_where_to_look() {
    let (home, _p, root) = synced(1);
    let out = recall_in(&root, home.path(), &["show", "zzzzzzzz"]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no archived session"), "said: {stderr}");
    assert!(stderr.contains("recall sessions"), "said: {stderr}");
}

#[test]
fn show_without_init_refuses_and_says_what_to_do() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let out = recall_in(project.path(), home.path(), &["show", "abc"]);

    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("recall init"));
}
