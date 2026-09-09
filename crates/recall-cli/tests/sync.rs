//! `recall sync`, end to end against a fake provider home.
//!
//! Nothing here reads a real `~/.claude` or a real project. The adapter takes
//! its root by injection precisely so these tests cannot.

use std::fs;
use std::path::Path;
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

/// A Claude Code session file for `project`, with `id`.
fn claude_session(home: &Path, project: &Path, id: &str, events: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");
    fs::write(dir.join(format!("{id}.jsonl")), events).expect("write session");
}

/// Two records: enough to be a session with a start and some content.
fn transcript(project: &Path, at: &str, text: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"{at}\",\"cwd\":\"{}\",\"gitBranch\":\"dev\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"cwd\":\"{}\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}\n",
        project.display(),
        project.display()
    )
}

fn archives_in(project: &Path) -> usize {
    walk(&project.join(".recall/sessions"))
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zst"))
        .count()
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
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

#[test]
fn a_session_from_this_project_is_archived() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    claude_session(
        home.path(),
        &root,
        "abc",
        &transcript(&root, "2026-09-08T12:00:00.000Z", "archive me"),
    );

    assert!(recall_in(&root, home.path(), &["init"]).status.success());
    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(
        out.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 archived"), "said: {stdout}");
    assert_eq!(archives_in(&root), 1);
}

#[test]
fn a_session_from_another_project_is_left_alone() {
    let home = tempfile::tempdir().expect("home");
    let mine = tempfile::tempdir().expect("project");
    let theirs = tempfile::tempdir().expect("other project");
    let root = mine.path().canonicalize().expect("canonical");
    let other = theirs.path().canonicalize().expect("canonical");

    claude_session(
        home.path(),
        &other,
        "theirs",
        &transcript(&other, "2026-09-08T12:00:00.000Z", "not yours"),
    );

    recall_in(&root, home.path(), &["init"]);
    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success());
    assert_eq!(
        archives_in(&root),
        0,
        "another project's conversation was archived here"
    );
}

#[test]
fn a_second_sync_archives_nothing_and_changes_nothing() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    claude_session(
        home.path(),
        &root,
        "abc",
        &transcript(&root, "2026-09-08T12:00:00.000Z", "once"),
    );
    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    let archive = walk(&root.join(".recall/sessions"))
        .into_iter()
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("zst"))
        .expect("an archive");
    let before = fs::read(&archive).expect("read");

    let out = recall_in(&root, home.path(), &["sync"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("0 archived"), "said: {stdout}");
    assert!(stdout.contains("1 already had"), "said: {stdout}");
    assert_eq!(archives_in(&root), 1, "a duplicate archive was created");
    assert_eq!(
        fs::read(&archive).expect("read"),
        before,
        "the archive was rewritten"
    );
}

#[test]
fn one_unreadable_session_does_not_stop_the_others() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    claude_session(
        home.path(),
        &root,
        "good-one",
        &transcript(&root, "2026-09-08T12:00:00.000Z", "fine"),
    );
    // Discovered, and unreadable: no record carries a timestamp.
    claude_session(
        home.path(),
        &root,
        "bad-one",
        "{\"type\":\"ai-title\",\"aiTitle\":\"nothing to date this by\"}\n",
    );
    claude_session(
        home.path(),
        &root,
        "good-two",
        &transcript(&root, "2026-09-08T13:00:00.000Z", "also fine"),
    );

    recall_in(&root, home.path(), &["init"]);
    let out = recall_in(&root, home.path(), &["sync"]);
    assert!(out.status.success(), "the run aborted on one bad session");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 archived"), "said: {stdout}");
    assert!(
        stdout.contains("could not be archived"),
        "the failure was not reported: {stdout}"
    );
    assert_eq!(archives_in(&root), 2);
}

#[test]
fn sync_never_modifies_the_providers_files() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    claude_session(
        home.path(),
        &root,
        "abc",
        &transcript(&root, "2026-09-08T12:00:00.000Z", "leave me alone"),
    );
    let provider_file = walk(&home.path().join(".claude"))
        .into_iter()
        .next()
        .expect("a provider file");
    let before = fs::read(&provider_file).expect("read");
    let mtime = fs::metadata(&provider_file)
        .expect("metadata")
        .modified()
        .ok();

    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);

    assert_eq!(fs::read(&provider_file).expect("read"), before);
    assert_eq!(
        fs::metadata(&provider_file)
            .expect("metadata")
            .modified()
            .ok(),
        mtime,
        "sync wrote to the provider's directory"
    );
}
