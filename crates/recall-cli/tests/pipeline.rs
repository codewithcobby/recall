//! The whole pipeline, from a provider fixture to a session read back out of
//! the archive.
//!
//! The other sync tests check that files appear. This one checks that what
//! appears is the conversation that went in — discovery, parsing,
//! normalization, compression and archiving, verified by decoding the result
//! rather than by counting files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use recall_core::{FileAction, Session, SessionEvent};
use recall_store::Archive;

/// A project with a Claude Code home beside it, and one fixture installed.
struct Fixture {
    home: tempfile::TempDir,
    /// Held only so the directory outlives the test. `root` is the path used.
    _project: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical");
        Self {
            home,
            _project: project,
            root,
        }
    }

    /// Install a session file, rewritten so its `cwd` is this project.
    ///
    /// The fixtures are committed with a placeholder path; sync matches
    /// sessions to a project by the working directory recorded inside them, so
    /// the fixture has to claim to have run here.
    fn with_session(&self, id: &str, fixture: &str) -> &Self {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../recall-adapters/tests/fixtures/claude")
            .join(format!("{fixture}.jsonl"));
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|e| panic!("read {}: {e}", source.display()))
            .replace("/w/demo", &self.root.display().to_string());

        let slug = self.root.display().to_string().replace('/', "-");
        let dir = self.home.path().join(".claude/projects").join(slug);
        fs::create_dir_all(&dir).expect("create project directory");
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, contents).expect("write session");

        // Sessions written moments ago are deliberately left alone.
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(long_ago)
            .expect("backdate");
        self
    }

    fn run(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_recall"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .output()
            .expect("run recall");
        assert!(
            out.status.success(),
            "recall {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Every session the archive holds, decoded.
    fn archived(&self) -> Vec<Session> {
        Archive::open(&self.root)
            .read_all()
            .expect("read the archive")
            .into_iter()
            .map(|r| r.expect("a session failed to read back"))
            .collect()
    }

    fn archive_files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![self.root.join(".recall/sessions")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
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
}

#[test]
fn a_conversation_survives_the_whole_pipeline() {
    let f = Fixture::new();
    f.with_session("ordinary", "ordinary");
    f.run(&["init"]);
    f.run(&["sync"]);

    let sessions = f.archived();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];

    // The metadata the adapter derived.
    assert_eq!(s.provider.as_str(), "claude-code");
    assert_eq!(s.provider_session_id, "ordinary");
    assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(s.project.as_deref(), Some(f.root.as_path()));
    assert_eq!(
        s.git.as_ref().and_then(|g| g.branch.as_deref()),
        Some("feature/demo")
    );

    // The conversation itself, in order.
    let kinds: Vec<_> = s.events.iter().map(|e| e.kind()).collect();
    assert_eq!(
        kinds,
        ["user", "assistant", "tool_call", "tool_result", "assistant"]
    );

    // And the content, not merely the shape.
    assert_eq!(
        s.events[0].searchable_text(),
        Some("add a retry to the fetch helper")
    );
    let SessionEvent::ToolCall { name, .. } = &s.events[2] else {
        panic!("expected a tool call");
    };
    assert_eq!(name, "read_file");
}

#[test]
fn the_archive_is_a_compressed_frame_on_disk() {
    let f = Fixture::new();
    f.with_session("ordinary", "ordinary");
    f.run(&["init"]);
    f.run(&["sync"]);

    let files = f.archive_files();
    assert_eq!(files.len(), 1);
    let archive = &files[0];
    assert_eq!(
        archive.extension().and_then(|e| e.to_str()),
        Some("zst"),
        "the archive is not named as compressed"
    );

    let bytes = fs::read(archive).expect("read");
    assert_eq!(
        &bytes[..4],
        &[0x28, 0xb5, 0x2f, 0xfd],
        "not a Zstandard frame"
    );
}

#[test]
fn the_archive_is_filed_by_utc_date_and_named_by_derived_id() {
    let f = Fixture::new();
    f.with_session("ordinary", "ordinary");
    f.run(&["init"]);
    f.run(&["sync"]);

    let archive = f.archive_files().remove(0);
    let relative = archive
        .strip_prefix(f.root.join(".recall/sessions"))
        .expect("under sessions/");

    // The fixture's first timestamp is 2026-09-08T12:00:00Z.
    assert!(
        relative.starts_with("2026/09/08"),
        "filed at {}",
        relative.display()
    );

    let stem = archive.file_stem().and_then(|s| s.to_str()).expect("stem");
    assert_eq!(stem.len(), 32, "not a derived id: {stem}");
    assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(stem, f.archived()[0].id.as_str());
}

#[test]
fn tool_activity_reaches_the_archive_intact() {
    let f = Fixture::new();
    f.with_session("commands", "commands_and_files");
    f.run(&["init"]);
    f.run(&["sync"]);

    let s = &f.archived()[0];
    let kinds: Vec<_> = s.events.iter().map(|e| e.kind()).collect();
    assert_eq!(
        kinds,
        [
            "command",
            "command",
            "file_change",
            "file_change",
            "file_change"
        ]
    );

    let actions: Vec<_> = s
        .events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::FileChange { action, .. } => Some(*action),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        [FileAction::Modified, FileAction::Read, FileAction::Modified]
    );

    // stderr is preserved rather than discarded as noise.
    let SessionEvent::Command { output, .. } = &s.events[1] else {
        panic!("expected a command");
    };
    assert!(output
        .as_deref()
        .unwrap_or_default()
        .contains("could not compile"));
}

#[test]
fn sub_agent_work_is_archived_with_its_session() {
    let f = Fixture::new();
    f.with_session("parent", "ordinary");

    // A sub-agent transcript beside it, timestamped inside the session.
    let slug = f.root.display().to_string().replace('/', "-");
    let subagents = f
        .home
        .path()
        .join(".claude/projects")
        .join(slug)
        .join("parent")
        .join("subagents");
    fs::create_dir_all(&subagents).expect("create subagents");
    let sub = subagents.join("task-a.jsonl");
    fs::write(
        &sub,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-08T12:00:07.000Z\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"sub-agent did this\"}]}}\n",
    )
    .expect("write subagent");
    fs::File::options()
        .write(true)
        .open(&sub)
        .expect("open")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .expect("backdate");

    f.run(&["init"]);
    f.run(&["sync"]);

    let sessions = f.archived();
    assert_eq!(sessions.len(), 1, "the sub-agent became its own session");

    let text: Vec<_> = sessions[0]
        .events
        .iter()
        .filter_map(|e| e.searchable_text())
        .collect();
    assert!(
        text.contains(&"sub-agent did this"),
        "sub-agent work never reached the archive: {text:?}"
    );
}

#[test]
fn several_sessions_are_archived_together_and_read_back_separately() {
    let f = Fixture::new();
    f.with_session("one", "ordinary");
    f.with_session("two", "thinking");
    f.with_session("three", "string_content");
    f.run(&["init"]);

    let output = f.run(&["sync"]);
    assert!(output.contains("3 archived"), "said: {output}");

    let sessions = f.archived();
    assert_eq!(sessions.len(), 3);

    let mut ids: Vec<_> = sessions
        .iter()
        .map(|s| s.provider_session_id.clone())
        .collect();
    ids.sort();
    assert_eq!(ids, ["one", "three", "two"]);

    // Each is its own conversation, not a merge of all three.
    for s in &sessions {
        assert!(s.event_count() > 0);
        assert_eq!(
            s.id,
            recall_core::SessionId::derive(&s.provider, &s.provider_session_id)
        );
    }
}

#[test]
fn a_session_that_cannot_be_read_is_reported_and_the_rest_still_land() {
    let f = Fixture::new();
    f.with_session("good", "ordinary");
    f.with_session("undateable", "no_timestamps");
    f.with_session("also-good", "thinking");
    f.run(&["init"]);

    let output = f.run(&["sync"]);
    assert!(output.contains("2 archived"), "said: {output}");
    assert!(
        output.contains("could not be archived"),
        "the failure was not reported: {output}"
    );
    assert_eq!(f.archived().len(), 2);
}

#[test]
fn syncing_twice_leaves_the_archive_byte_identical() {
    let f = Fixture::new();
    f.with_session("ordinary", "ordinary");
    f.run(&["init"]);
    f.run(&["sync"]);

    let archive = f.archive_files().remove(0);
    let before = fs::read(&archive).expect("read");

    let output = f.run(&["sync"]);
    assert!(output.contains("1 already had"), "said: {output}");
    assert_eq!(f.archive_files().len(), 1);
    assert_eq!(fs::read(&archive).expect("read"), before);
}
