//! Archive a session in a real directory, read it back, then break it.
//!
//! Run with `cargo run --example archive_roundtrip`.
//!
//! Phase 3 has no CLI surface — no `recall` command touches the archive until
//! `recall sync` in #24 — so this is how a person sees what an archived session
//! looks like on disk. `docs/manual-testing.md` walks through the output.
//!
//! Everything happens in a temporary directory that is removed on exit. No real
//! project, provider directory, or home directory is touched.

use std::fs;
use std::path::Path;

use recall_core::{FileAction, Provider, Session, SessionEvent, SessionId};
use recall_store::{Archive, ArchiveError};
use time::macros::datetime;

fn main() {
    let project = tempfile::tempdir().expect("temporary project directory");
    recall_store::init(project.path()).expect("recall init");
    let archive = Archive::open(project.path());

    // 1. Archive a session.
    let session = demo_session("abc-123", datetime!(2026-09-08 12:00:00 UTC));
    let stored = archive.write(&session).expect("write");

    println!("== 1. archiving a session ==");
    println!("  session id   {}", session.id);
    println!("  archived at  {}", relative(stored.path(), project.path()));
    println!("  newly written {}", stored.is_new());

    let mut encoded = Vec::new();
    recall_core::write_session(&mut encoded, &session).expect("encode");
    let on_disk = fs::metadata(stored.path()).expect("metadata").len();
    println!(
        "  compressed   {} bytes encoded -> {on_disk} on disk ({:.1}x)",
        encoded.len(),
        encoded.len() as f64 / on_disk as f64
    );

    println!("\n== 2. what is on disk ==");
    for path in walk(&project.path().join(".recall")) {
        let meta = fs::symlink_metadata(&path).expect("metadata");
        println!(
            "  {:<11} {}",
            mode_of(&path),
            relative(&path, project.path()) + if meta.is_dir() { "/" } else { "" }
        );
    }

    let staged: Vec<_> = fs::read_dir(archive.layout().tmp())
        .expect("read tmp")
        .filter_map(|e| e.ok())
        .collect();
    println!(
        "  staging holds {} file(s)  <- anything here would be an interrupted write",
        staged.len()
    );

    // 3. Writing the same session again must not rewrite it.
    let before = fs::read(stored.path()).expect("read");
    let again = archive.write(&session).expect("second write");
    let after = fs::read(stored.path()).expect("read");
    println!("\n== 3. writing the same session twice ==");
    println!("  reported as new:      {}", again.is_new());
    println!("  bytes on disk changed: {}", before != after);

    // 4. Read it back.
    let recovered = archive.read(&session.id).expect("read back");
    println!("\n== 4. reading it back ==");
    println!("  identical to what we archived: {}", recovered == session);
    println!(
        "  events recovered:              {}",
        recovered.event_count()
    );

    // 5. Break it, one way at a time.
    println!("\n== 5. what happens to a damaged archive ==");
    let path = stored.path().to_path_buf();
    let good = fs::read(&path).expect("read");

    let root = project.path();

    // Damage at the compression layer.
    let half = &good[..good.len() / 2];
    damage(&archive, &session.id, &path, "truncated frame", half, root);
    damage(&archive, &session.id, &path, "emptied", b"", root);
    damage(
        &archive,
        &session.id,
        &path,
        "garbage bytes",
        &[0xff, 0x00, 0x42],
        root,
    );

    // A single flipped bit inside an otherwise valid frame. Zstandard
    // checksums its frames, so this is caught rather than decoded into
    // whatever the corrupted bytes happen to mean.
    let mut flipped = good.clone();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0x01;
    damage(
        &archive,
        &session.id,
        &path,
        "one flipped bit",
        &flipped,
        root,
    );

    // Damage inside the session, under a perfectly valid frame: the archive
    // decompresses cleanly and the session it holds is from a newer Recall.
    let plain = zstd::decode_all(good.as_slice()).expect("decompress");
    let future =
        String::from_utf8(plain)
            .expect("utf8")
            .replacen("\"format\":1", "\"format\":99", 1);
    let repacked = zstd::encode_all(future.as_bytes(), 1).expect("recompress");
    damage(
        &archive,
        &session.id,
        &path,
        "future version",
        &repacked,
        root,
    );

    fs::write(&path, &good).expect("restore");
    fs::remove_file(&path).expect("remove");
    report("missing", archive.read(&session.id), root);
    fs::create_dir(&path).expect("directory in its place");
    report("a directory", archive.read(&session.id), root);
    fs::remove_dir(&path).expect("cleanup");
    fs::write(&path, &good).expect("restore");

    // 6. One bad archive must not hide the others.
    println!("\n== 6. one damaged archive among several ==");
    let second = demo_session("def-456", datetime!(2026-09-07 09:00:00 UTC));
    let third = demo_session("ghi-789", datetime!(2026-09-06 18:00:00 UTC));
    let broken = archive.write(&second).expect("write").path().to_path_buf();
    archive.write(&third).expect("write");
    fs::write(&broken, b"{not json at all").expect("damage");

    let results = archive.read_all().expect("read all");
    let recovered = results.iter().filter(|r| r.is_ok()).count();
    let failed = results.len() - recovered;
    println!("  archives found:    {}", results.len());
    println!("  read successfully: {recovered}");
    println!("  reported broken:   {failed}");
    for error in results.iter().filter_map(|r| r.as_ref().err()) {
        println!("    {}", tidy(&error.to_string(), project.path()));
    }

    println!("\nEvery line in section 5 should say \"refused\". An \"ACCEPTED\" is a bug.");
    println!("In section 6, two archives must still read despite the third being broken.");
}

/// Overwrite the archive with something broken and report what reading does.
fn damage(
    archive: &Archive,
    id: &SessionId,
    path: &Path,
    what: &str,
    bytes: &[u8],
    project: &Path,
) {
    fs::write(path, bytes).expect("damage the archive");
    report(what, archive.read(id), project);
}

/// Print whether a read was refused, and why.
///
/// The cause is the interesting half: the top-level error says which session
/// could not be read, and its source says what is actually wrong with the file.
fn report(what: &str, result: Result<Session, ArchiveError>, project: &Path) {
    match result {
        Ok(session) => println!(
            "  {what:<16} ACCEPTED as a session with {} events  <- this would be a bug",
            session.event_count()
        ),
        Err(e) => {
            let cause = std::error::Error::source(&e)
                .map(|c| format!("  ({c})"))
                .unwrap_or_default();
            println!(
                "  {what:<16} refused: {}{cause}",
                tidy(&e.to_string(), project)
            );
        }
    }
}

/// Replace the temporary project path so the output stays readable.
fn tidy(message: &str, project: &Path) -> String {
    match project.to_str() {
        Some(prefix) => message.replace(prefix, "<project>"),
        None => message.to_string(),
    }
}

/// A session with one of every event variant.
fn demo_session(provider_id: &str, at: time::OffsetDateTime) -> Session {
    let mut session = Session::new(
        Provider::new("claude-code").expect("valid provider"),
        provider_id,
        at,
    );
    session.model = Some("claude-opus-5".into());
    session.ended_at = Some(at + time::Duration::minutes(90));
    session.project = Some("/home/me/project".into());
    session.events = vec![
        SessionEvent::UserMessage {
            at: Some(at),
            content: "refactor the payment orchestrator".into(),
        },
        SessionEvent::AssistantMessage {
            at: None,
            content: "Reading the current implementation first.".into(),
        },
        SessionEvent::ToolCall {
            at: None,
            name: "read_file".into(),
            arguments: Some(r#"{"path":"src/payments.rs"}"#.into()),
            call_id: Some("call-1".into()),
        },
        SessionEvent::ToolResult {
            at: None,
            call_id: Some("call-1".into()),
            content: "pub fn charge() {}".into(),
            failed: Some(false),
        },
        SessionEvent::Command {
            at: None,
            command: "cargo test".into(),
            exit_code: Some(0),
            output: Some("ok".into()),
        },
        SessionEvent::FileChange {
            at: None,
            action: FileAction::Modified,
            path: "src/payments.rs".into(),
        },
    ];
    session
}

/// Every path under `root`, depth first, sorted.
fn walk(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// The permission bits, where the platform has them.
#[cfg(unix)]
fn mode_of(path: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    format!("{mode:04o}")
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> String {
    "(n/a)".to_string()
}

/// A path relative to the project, for readable output.
fn relative(path: &Path, project: &Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .display()
        .to_string()
}
