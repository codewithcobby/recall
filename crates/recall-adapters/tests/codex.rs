//! The Codex adapter, end to end over synthetic fixtures.
//!
//! Every fixture is hand-written from `docs/providers/codex.md`. No real
//! transcript is committed to this repository, and none was used to build them
//! — only the format's structure, confirmed against a real installation.

use std::fs;
use std::path::{Path, PathBuf};

use recall_adapters::Codex;
use recall_core::{Adapter, AdapterError, Session, SessionEvent};

/// The date directory the fixtures are filed under.
const DATE: [&str; 3] = ["2026", "08", "03"];

/// Build a fake `.codex` directory containing one fixture as a rollout.
///
/// The fixture is placed where Codex would put it, so discovery has to walk the
/// date tree to find it rather than being handed the path.
fn home_with(fixture: &str) -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("temp dir");
    let dir = home
        .path()
        .join("sessions")
        .join(DATE[0])
        .join(DATE[1])
        .join(DATE[2]);
    fs::create_dir_all(&dir).expect("create date directory");

    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex")
        .join(format!("{fixture}.jsonl"));
    let destination =
        dir.join("rollout-2026-08-03T13-10-00-019fc7bf-5907-7222-a195-fa7ee3f9556f.jsonl");
    fs::copy(&source, &destination).unwrap_or_else(|e| panic!("copy {}: {e}", source.display()));

    (home, destination)
}

/// Load one fixture through the whole adapter.
fn load(fixture: &str) -> Result<Session, AdapterError> {
    let (home, path) = home_with(fixture);
    let adapter = Codex::rooted_at(home.path());
    let found = adapter.discover().expect("discover");
    let discovered = found
        .iter()
        .find(|d| d.path == path)
        .unwrap_or_else(|| panic!("{fixture} was not discovered"));
    adapter.load(discovered)
}

fn kinds(session: &Session) -> Vec<&'static str> {
    session.events.iter().map(|e| e.kind()).collect()
}

fn text(session: &Session) -> Vec<&str> {
    session
        .events
        .iter()
        .filter_map(|e| e.searchable_text())
        .collect()
}

#[test]
fn an_ordinary_session_becomes_a_session() {
    let s = load("ordinary").expect("load");

    assert_eq!(s.provider.as_str(), "codex");
    assert_eq!(
        s.provider_session_id,
        "019fc7bf-5907-7222-a195-fa7ee3f9556f"
    );
    assert_eq!(s.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(s.project, Some(PathBuf::from("/w/demo")));
    assert_eq!(s.duration().map(|d| d.whole_minutes()), Some(30));
    assert_eq!(kinds(&s), ["user", "assistant", "user"]);
}

#[test]
fn a_codex_session_reports_no_git_context() {
    // Codex records no branch and no commit. Claiming one would be invention.
    let s = load("ordinary").expect("load");
    assert_eq!(s.git, None);
}

#[test]
fn the_session_is_filed_by_when_it_started_not_when_it_was_synced() {
    let s = load("ordinary").expect("load");
    assert_eq!(s.archive_date(), (2026, 8, 3));
}

#[test]
fn a_shell_command_is_recorded_as_the_command_it_was() {
    // The thing the Claude Code adapter cannot do: Codex records the command
    // line itself, so this answers "what did this agent run on my machine".
    let s = load("tool_calls").expect("load");

    let SessionEvent::Command {
        command,
        exit_code,
        output,
        ..
    } = &s.events[0]
    else {
        panic!("expected a command, got {:?}", kinds(&s));
    };
    assert_eq!(command, "cargo test --workspace");
    assert_eq!(*exit_code, None, "an exit code Codex never recorded");
    assert_eq!(
        *output, None,
        "output Codex records against the tool, not the command"
    );
}

#[test]
fn every_tool_call_keeps_its_arguments_and_pairs_with_its_result() {
    let s = load("tool_calls").expect("load");

    assert_eq!(
        kinds(&s),
        [
            "command",
            "tool_call",
            "tool_result",
            "tool_call",
            "tool_result",
            "tool_call",
            "tool_result"
        ]
    );

    let names: Vec<&str> = s
        .events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["exec_command", "view_image", "js"]);

    // A call and the result answering it share an id, which is what makes a
    // transcript readable back.
    let calls: Vec<&str> = s
        .events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolCall { call_id, .. } => call_id.as_deref(),
            _ => None,
        })
        .collect();
    let results: Vec<&str> = s
        .events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolResult { call_id, .. } => call_id.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(calls, ["call-1", "call-2", "call-3"]);
    assert_eq!(results, calls);
}

#[test]
fn a_tool_calls_arguments_are_preserved_exactly_as_recorded() {
    let s = load("tool_calls").expect("load");
    let SessionEvent::ToolCall { arguments, .. } = &s.events[1] else {
        panic!("expected a tool call");
    };
    let arguments = arguments.as_deref().expect("arguments");
    assert!(
        arguments.contains(r#""cmd":"cargo test --workspace""#),
        "{arguments}"
    );
    // Fields the adapter has no use for are still there, because the value is
    // kept verbatim rather than re-encoded from what was understood.
    assert!(arguments.contains("max_output_tokens"), "{arguments}");
}

#[test]
fn a_structured_tool_result_is_not_flattened_into_nothing() {
    let s = load("tool_calls").expect("load");
    let SessionEvent::ToolResult { content, .. } = &s.events[4] else {
        panic!("expected a tool result");
    };
    assert_eq!(content, "a diagram of the pipeline");
}

#[test]
fn no_tool_result_claims_to_know_whether_it_failed() {
    // Codex records no status on the record. Some(false) would claim a success
    // it never stated.
    let s = load("tool_calls").expect("load");
    for event in &s.events {
        if let SessionEvent::ToolResult { failed, .. } = event {
            assert_eq!(*failed, None);
        }
    }
}

#[test]
fn codexs_own_interface_records_are_not_archived() {
    // event_msg mirrors the response_item stream for the UI, and world_state is
    // Codex's snapshot of the workspace. Keeping them would archive the same
    // message twice and bury the conversation in bookkeeping.
    let s = load("bookkeeping").expect("load");
    assert_eq!(kinds(&s), ["assistant"]);
    assert_eq!(text(&s), ["the only real message"]);
}

#[test]
fn reasoning_is_kept_but_its_encrypted_payload_is_not() {
    let s = load("reasoning_and_roles").expect("load");

    assert!(
        text(&s).contains(&"weighing two ways to name the flag"),
        "reasoning was dropped: {:?}",
        text(&s)
    );
    assert!(
        !text(&s).iter().any(|t| t.contains("opaque-and-unreadable")),
        "opaque encrypted content was archived"
    );
}

#[test]
fn a_developer_message_is_not_filed_as_the_person_or_the_model() {
    // It is neither. Filing it under one of the two would misattribute it.
    let s = load("reasoning_and_roles").expect("load");
    let kinds = kinds(&s);
    assert_eq!(
        kinds.iter().filter(|k| **k == "user").count(),
        0,
        "a developer message was archived as the person: {kinds:?}"
    );

    let developer = s.events.iter().any(
        |e| matches!(e, SessionEvent::Other { provider_kind, .. } if provider_kind == "developer"),
    );
    assert!(developer, "the developer message was lost: {kinds:?}");
}

#[test]
fn the_system_prompt_the_session_ran_under_is_kept() {
    let s = load("reasoning_and_roles").expect("load");
    assert!(
        text(&s).contains(&"You are a coding agent. Be pragmatic."),
        "the system prompt was dropped"
    );
}

#[test]
fn shapes_this_build_has_never_seen_are_kept_rather_than_dropped() {
    // A Codex history spans a year of releases, so this is the ordinary case
    // rather than an edge one.
    let s = load("unknown_shapes").expect("load");

    let unknown: Vec<&str> = s
        .events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Other { provider_kind, .. } => Some(provider_kind.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        unknown,
        [
            "web_search_call",
            "tool_search_call",
            "a_type_added_after_this_adapter_was_written"
        ],
        "an unrecognised item was dropped"
    );

    // And a known item carrying an unknown field is still understood.
    assert!(text(&s).contains(&"still here"));
}

#[test]
fn a_broken_line_costs_that_line_and_nothing_else() {
    let s = load("malformed_middle").expect("load");
    assert_eq!(text(&s), ["before the break", "after the break"]);
}

#[test]
fn a_rollout_still_being_written_yields_everything_written_so_far() {
    // The ordinary case for the session running right now: its last line is
    // half-written. Losing the session over it would be the worst outcome.
    let s = load("truncated").expect("load");
    assert_eq!(text(&s), ["this one finished writing"]);
}

#[test]
fn a_rollout_with_nothing_to_date_it_by_is_not_archived() {
    // The archive files by start date. Substituting "now" would file the
    // session under the day it was synced rather than the day it happened.
    let err = load("no_timestamps").expect_err("should not load");
    assert!(matches!(err, AdapterError::Empty { .. }), "{err:?}");
}

#[test]
fn an_empty_rollout_holds_no_session() {
    let err = load("empty").expect_err("should not load");
    assert!(matches!(err, AdapterError::Empty { .. }), "{err:?}");
}

#[test]
fn one_unreadable_rollout_does_not_stop_the_others_being_found() {
    // The rule from CONTRIBUTING: one bad file never fails the run. Discovery
    // reports all three; only the one that cannot be read fails to load.
    let home = tempfile::tempdir().expect("temp dir");
    let dir = home
        .path()
        .join("sessions")
        .join(DATE[0])
        .join(DATE[1])
        .join(DATE[2]);
    fs::create_dir_all(&dir).expect("mkdir");

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex");
    for (fixture, uuid) in [
        ("ordinary", "aaaaaaaa-1111-2222-3333-444444444444"),
        ("empty", "bbbbbbbb-1111-2222-3333-444444444444"),
        ("tool_calls", "cccccccc-1111-2222-3333-444444444444"),
    ] {
        fs::copy(
            fixtures.join(format!("{fixture}.jsonl")),
            dir.join(format!("rollout-2026-08-03T13-10-00-{uuid}.jsonl")),
        )
        .expect("copy");
    }

    let adapter = Codex::rooted_at(home.path());
    let found = adapter.discover().expect("discover");
    assert_eq!(found.len(), 3, "found {found:?}");

    let loaded = found.iter().filter(|d| adapter.load(d).is_ok()).count();
    assert_eq!(
        loaded, 2,
        "a readable session was lost to an unreadable one"
    );
}

#[test]
fn the_codex_version_a_rollout_was_written_by_is_reported() {
    // Formats move between releases, so this is what a parse failure gets
    // diagnosed against.
    let (home, path) = home_with("ordinary");
    let adapter = Codex::rooted_at(home.path());
    assert_eq!(adapter.provider_version(&path).as_deref(), Some("0.152.1"));
}

#[test]
fn loading_a_session_leaves_the_rollout_untouched() {
    // Provider files are read-only, always — see .github/SECURITY.md.
    let (home, path) = home_with("tool_calls");
    let before = fs::read(&path).expect("read");
    let modified_before = fs::metadata(&path).expect("metadata").modified().ok();

    let adapter = Codex::rooted_at(home.path());
    for discovered in adapter.discover().expect("discover") {
        let _ = adapter.load(&discovered);
    }

    assert_eq!(fs::read(&path).expect("read"), before);
    assert_eq!(
        fs::metadata(&path).expect("metadata").modified().ok(),
        modified_before,
        "loading modified the provider's file"
    );
}

#[test]
fn the_same_rollout_always_archives_under_the_same_id() {
    // This is what lets sync recognise a session it has already archived.
    let first = load("ordinary").expect("load");
    let again = load("ordinary").expect("load");
    assert_eq!(first.id, again.id);
}
