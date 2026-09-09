//! The Claude Code adapter, end to end over synthetic fixtures.
//!
//! Every fixture is hand-written from `docs/providers/claude-code.md`. No real
//! transcript is committed to this repository, and none was used to build them
//! — only the format's structure, confirmed against a real installation.

use std::fs;
use std::path::{Path, PathBuf};

use recall_adapters::ClaudeCode;
use recall_core::{Adapter, AdapterError, FileAction, Session, SessionEvent};

/// Build a fake `.claude` directory containing one fixture as a session.
fn home_with(fixture: &str) -> (tempfile::TempDir, String) {
    let home = tempfile::tempdir().expect("temp dir");
    let project = home.path().join("projects").join("-w-demo");
    fs::create_dir_all(&project).expect("create project directory");

    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude")
        .join(format!("{fixture}.jsonl"));
    let id = fixture.replace('_', "-");
    fs::copy(&source, project.join(format!("{id}.jsonl")))
        .unwrap_or_else(|e| panic!("copy {}: {e}", source.display()));

    (home, id)
}

/// Load one fixture through the whole adapter.
fn load(fixture: &str) -> Result<Session, AdapterError> {
    let (home, id) = home_with(fixture);
    let adapter = ClaudeCode::rooted_at(home.path());
    let found = adapter.discover().expect("discover");
    let discovered = found
        .iter()
        .find(|d| d.provider_session_id == id)
        .unwrap_or_else(|| panic!("{fixture} was not discovered"));
    adapter.load(discovered)
}

fn kinds(session: &Session) -> Vec<&'static str> {
    session.events.iter().map(|e| e.kind()).collect()
}

#[test]
fn an_ordinary_session_becomes_a_session() {
    let s = load("ordinary").expect("load");

    assert_eq!(s.provider.as_str(), "claude-code");
    assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(s.project, Some(PathBuf::from("/w/demo")));
    assert_eq!(
        s.git.as_ref().and_then(|g| g.branch.as_deref()),
        Some("feature/demo")
    );
    assert_eq!(s.duration().map(|d| d.whole_minutes()), Some(30));
    assert_eq!(
        kinds(&s),
        ["user", "assistant", "tool_call", "tool_result", "assistant"]
    );
}

#[test]
fn claude_codes_own_records_are_not_archived() {
    // 17 of the 21 record types are internal state. Only the one real message
    // should survive.
    let s = load("bookkeeping").expect("load");
    assert_eq!(kinds(&s), ["user"]);
    assert_eq!(s.events[0].searchable_text(), Some("the only real message"));
}

#[test]
fn content_is_read_in_both_of_its_shapes() {
    let s = load("string_content").expect("load");
    assert_eq!(kinds(&s), ["user", "assistant"]);
    assert_eq!(
        s.events[0].searchable_text(),
        Some("content as a bare string")
    );
    assert_eq!(s.events[1].searchable_text(), Some("content as an array"));
}

#[test]
fn tool_results_recorded_as_text_are_kept() {
    // This shape dropped 1,061 real records before it was handled.
    let s = load("text_tool_result").expect("load");
    assert_eq!(kinds(&s), ["tool_result", "tool_result"]);
    let SessionEvent::ToolResult {
        content, failed, ..
    } = &s.events[0]
    else {
        panic!("expected a tool result");
    };
    assert_eq!(content, "User rejected tool use");
    assert_eq!(*failed, Some(true));
}

#[test]
fn commands_and_file_changes_are_derived() {
    let s = load("commands_and_files").expect("load");
    assert_eq!(
        kinds(&s),
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
    // Patched, read-only, and replaced.
    assert_eq!(
        actions,
        [FileAction::Modified, FileAction::Read, FileAction::Modified]
    );

    // stderr is preserved, not discarded because the command "failed".
    let SessionEvent::Command { output, .. } = &s.events[1] else {
        panic!("expected a command");
    };
    assert!(output
        .as_deref()
        .unwrap_or_default()
        .contains("could not compile"));
}

#[test]
fn extended_thinking_is_preserved() {
    let s = load("thinking").expect("load");
    assert_eq!(kinds(&s), ["other", "assistant"]);
    let SessionEvent::Other {
        provider_kind,
        content,
        ..
    } = &s.events[0]
    else {
        panic!("expected an other event");
    };
    assert_eq!(provider_kind, "thinking");
    assert!(content.contains("weighing two designs"));
}

#[test]
fn internal_reminders_are_skipped_and_real_attachments_kept() {
    let s = load("attachments").expect("load");
    assert_eq!(s.event_count(), 2, "got {:?}", kinds(&s));

    let labels: Vec<_> = s
        .events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Other { provider_kind, .. } => Some(provider_kind.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            "attachment:edited_text_file:src/fetch.rs",
            "attachment:an_unknown_kind"
        ]
    );
}

#[test]
fn shapes_from_a_future_release_are_kept_not_fatal() {
    // A new record type, a new content block, and a new field. None of them
    // should cost the user the rest of the session.
    let s = load("unknown_shapes").expect("load");
    let ks = kinds(&s);
    assert!(ks.contains(&"assistant"), "got {ks:?}");
    assert!(ks.contains(&"user"), "got {ks:?}");
    assert!(
        ks.contains(&"other"),
        "the unrecognised block was dropped: {ks:?}"
    );
}

#[test]
fn a_broken_line_costs_only_that_line() {
    let s = load("malformed_middle").expect("load");
    let text: Vec<_> = s
        .events
        .iter()
        .filter_map(|e| e.searchable_text())
        .collect();
    assert_eq!(text, ["before the break", "after the break"]);
}

#[test]
fn a_truncated_file_keeps_what_was_complete() {
    let s = load("truncated").expect("load");
    assert_eq!(
        s.events[0].searchable_text(),
        Some("complete record"),
        "the complete record was lost with the incomplete one"
    );
}

#[test]
fn a_session_that_cannot_be_dated_is_refused() {
    // The archive files by start date. Inventing one would file it wrongly
    // forever.
    assert!(matches!(
        load("no_timestamps"),
        Err(AdapterError::Empty { .. })
    ));
}

#[test]
fn an_empty_file_is_refused_rather_than_archived_as_an_empty_session() {
    assert!(matches!(load("empty"), Err(AdapterError::Empty { .. })));
}

#[test]
fn sub_agent_transcripts_are_folded_into_their_session() {
    let home = tempfile::tempdir().expect("temp dir");
    let project = home.path().join("projects").join("-w-demo");
    let subagents = project.join("parent").join("subagents");
    fs::create_dir_all(&subagents).expect("create directories");

    fs::write(
        project.join("parent.jsonl"),
        "{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"cwd\":\"/w/demo\",\"message\":{\"role\":\"user\",\"content\":\"main thread first\"}}\n\
         {\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:20.000Z\",\"cwd\":\"/w/demo\",\"message\":{\"role\":\"user\",\"content\":\"main thread last\"}}\n",
    )
    .expect("write parent");
    fs::write(
        subagents.join("task-a.jsonl"),
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-08T12:00:10.000Z\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"sub-agent in the middle\"}]}}\n",
    )
    .expect("write subagent");

    let adapter = ClaudeCode::rooted_at(home.path());
    let found = adapter.discover().expect("discover");
    assert_eq!(found.len(), 1, "sub-agent reported as its own session");
    assert_eq!(found[0].additional_paths.len(), 1);

    let s = adapter.load(&found[0]).expect("load");
    let text: Vec<_> = s
        .events
        .iter()
        .filter_map(|e| e.searchable_text())
        .collect();
    assert_eq!(
        text,
        [
            "main thread first",
            "sub-agent in the middle",
            "main thread last"
        ],
        "sub-agent work was not interleaved by time"
    );
}

#[test]
fn loading_never_modifies_the_provider_directory() {
    let (home, id) = home_with("ordinary");
    let path = home
        .path()
        .join("projects/-w-demo")
        .join(format!("{id}.jsonl"));
    let before = fs::read(&path).expect("read");
    let mtime = fs::metadata(&path).expect("metadata").modified().ok();

    let adapter = ClaudeCode::rooted_at(home.path());
    for d in adapter.discover().expect("discover") {
        let _ = adapter.load(&d);
    }

    assert_eq!(fs::read(&path).expect("read"), before);
    assert_eq!(
        fs::metadata(&path).expect("metadata").modified().ok(),
        mtime,
        "the adapter wrote to the provider's directory"
    );
}

#[test]
fn every_fixture_is_handled_without_panicking() {
    for fixture in [
        "ordinary",
        "bookkeeping",
        "string_content",
        "text_tool_result",
        "commands_and_files",
        "thinking",
        "attachments",
        "unknown_shapes",
        "malformed_middle",
        "truncated",
        "no_timestamps",
        "empty",
    ] {
        let _ = load(fixture);
    }
}
