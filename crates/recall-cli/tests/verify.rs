//! `recall verify`.
//!
//! The question `recall sessions` cannot answer: are the archives still
//! readable all the way through?

use std::fs;
use std::path::{Path, PathBuf};
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

/// A settled session with enough events that damage can land past the header.
fn claude_session(home: &Path, project: &Path, id: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("mkdir");
    let mut lines = String::new();
    for i in 0..400 {
        lines.push_str(&format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":\"event {i} with enough text to compress\"}}}}\n",
            project.display()
        ));
    }
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(&path, lines).expect("write");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .expect("backdate");
}

fn archives(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.join(".recall/sessions")];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("zst") {
                found.push(p);
            }
        }
    }
    found.sort();
    found
}

/// Break a session's transcript while leaving its header byte-identical.
///
/// Decompresses, corrupts a byte well past the first newline, and recompresses.
/// Flipping a byte in the compressed file instead is a coin toss: the header is
/// unique text that compresses poorly, so it occupies a large share of a small
/// frame, and a randomly-placed flip lands inside it often enough to make a
/// test fail on some machines and pass on others (#140).
fn damage_the_body(archive: &Path) {
    let raw = zstd::decode_all(fs::File::open(archive).expect("open")).expect("decompress");
    let header_end = raw.iter().position(|b| *b == b'\n').expect("a header line") + 1;
    assert!(
        raw.len() > header_end + 16,
        "the fixture has no transcript to damage"
    );

    let mut damaged = raw.clone();
    // Comfortably inside the transcript, never the header.
    let target = header_end + (raw.len() - header_end) / 2;
    damaged[target] ^= 0xff;
    assert_eq!(
        &damaged[..header_end],
        &raw[..header_end],
        "the header was altered"
    );

    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 12).expect("encoder");
    encoder.include_checksum(true).expect("checksum");
    std::io::Write::write_all(&mut encoder, &damaged).expect("compress");
    fs::write(archive, encoder.finish().expect("finish")).expect("write");
}

/// A project with `count` archived sessions.
fn archived(count: usize) -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    for i in 0..count {
        claude_session(home.path(), &root, &format!("session-{i}"));
    }
    recall_in(&root, home.path(), &["init"]);
    recall_in(&root, home.path(), &["sync"]);
    (home, project, root)
}

#[test]
fn a_healthy_archive_verifies() {
    let (home, _p, root) = archived(3);
    let out = recall_in(&root, home.path(), &["verify"]);

    assert!(out.status.success(), "exit {:?}", out.status.code());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("3 sessions checked: 3 readable, 0 damaged"),
        "said: {stdout}"
    );
}

#[test]
fn corruption_after_the_header_is_caught_where_listing_is_blind() {
    // The exact gap this command exists to close.
    let (home, _p, root) = archived(3);

    damage_the_body(&archives(&root).remove(1));

    // The listing cannot see it: the header is intact.
    let listed = recall_in(&root, home.path(), &["sessions"]);
    assert!(listed.status.success());
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("3 sessions"),
        "the listing did not report all three"
    );

    // Verification can.
    let out = recall_in(&root, home.path(), &["verify"]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "a damaged archive did not exit 7"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 readable, 1 damaged"), "said: {stdout}");
}

#[test]
fn the_reason_reaches_the_user() {
    let (home, _p, root) = archived(1);
    damage_the_body(&archives(&root).remove(0));

    let out = recall_in(&root, home.path(), &["verify"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 damaged"), "said: {stdout}");

    // The report must carry the reason underneath the category, not only the
    // category. *Which* reason depends on where the damaged byte landed:
    // Zstandard's frame checksum catches some, and some decompress into
    // something that is no longer a session. Asserting the library's exact
    // wording would make this test depend on where a random byte fell — which
    // it did, and it failed about four runs in five until this was fixed.
    let reported = stdout
        .lines()
        .find(|l| l.contains("is corrupt") || l.contains("not readable as a session"))
        .unwrap_or_else(|| panic!("no damaged archive was described: {stdout}"));

    assert!(
        reported.matches(": ").count() >= 1,
        "the category was reported without a cause: {reported}"
    );
}

#[test]
fn one_session_can_be_verified_on_its_own() {
    let (home, _p, root) = archived(3);
    let listed = recall_in(&root, home.path(), &["sessions"]);
    let id = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("an id")
        .to_string();

    let out = recall_in(&root, home.path(), &["verify", &id]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 session checked"),
        "it checked more than the one asked for"
    );
}

#[test]
fn verifying_an_unknown_session_says_so() {
    let (home, _p, root) = archived(1);
    let out = recall_in(&root, home.path(), &["verify", "zzzzzzzz"]);
    assert_eq!(out.status.code(), Some(5));
}

#[test]
fn verify_without_init_refuses() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let out = recall_in(project.path(), home.path(), &["verify"]);
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn an_empty_archive_verifies_without_complaint() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    recall_in(project.path(), home.path(), &["init"]);

    let out = recall_in(project.path(), home.path(), &["verify"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("No sessions archived yet"));
}
