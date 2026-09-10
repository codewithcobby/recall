//! Turning Codex's records into Recall's model.
//!
//! The rule throughout is the one from `.github/CONTRIBUTING.md`: preserve what
//! is there, derive what follows from it, invent nothing. A field Codex does
//! not record stays `None`.
//!
//! What Codex can supply that Claude Code cannot: the command line itself. An
//! `exec_command` call carries `cmd`, so a [`SessionEvent::Command`] here names
//! what was actually run rather than only what it printed.
//!
//! What it cannot supply: the git branch, which Claude Code records on every
//! record and Codex never does. Sessions from Codex therefore carry no
//! [`recall_core::GitContext`] — #32 derives that from the repository instead,
//! and guessing it here would be invention.

use std::path::PathBuf;

use recall_core::{AdapterError, Provider, Session, SessionEvent, SessionId};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::parse::ParsedFile;
use super::record::{ContentItem, ExecCommandArguments, Item, KnownItem, EXEC_COMMAND_TOOL};

/// Build a session from a parsed rollout.
///
/// One rollout is one session: unlike Claude Code there are no sibling
/// transcripts to merge, so file order is the transcript's order.
pub fn normalize(
    provider: &Provider,
    provider_session_id: &str,
    project_hint: Option<PathBuf>,
    file: &ParsedFile,
) -> Result<Session, AdapterError> {
    let mut events = Vec::new();
    let mut model = None;
    let mut cwd = None;
    let mut earliest: Option<OffsetDateTime> = None;
    let mut latest: Option<OffsetDateTime> = None;

    for record in &file.records {
        let at = record.timestamp.as_deref().and_then(parse_timestamp);
        if let Some(at) = at {
            earliest = Some(earliest.map_or(at, |e| e.min(at)));
            latest = Some(latest.map_or(at, |l| l.max(at)));
        }

        if let Some(meta) = record.session_meta() {
            if cwd.is_none() {
                cwd = meta.cwd.filter(|c| !c.is_empty()).map(PathBuf::from);
            }
            // The system prompt is what the model was actually told, so it is
            // transcript rather than bookkeeping. It is also the largest field
            // in the file, which is a reason to compress it, not to drop it.
            if let Some(text) = meta.base_instructions.as_ref().and_then(|b| b.text()) {
                events.push(SessionEvent::Other {
                    at,
                    provider_kind: "base_instructions".to_string(),
                    content: text.to_string(),
                });
            }
            continue;
        }

        if let Some(context) = record.turn_context() {
            // First model wins. A session can switch models mid-way — a review
            // pass runs under a different one — and the session's model is the
            // one it started under.
            if model.is_none() {
                model = context.model.filter(|m| !m.is_empty());
            }
            if cwd.is_none() {
                cwd = context.cwd.filter(|c| !c.is_empty()).map(PathBuf::from);
            }
            continue;
        }

        if let Some(item) = record.response_item() {
            item_events(&item, at, &mut events);
            continue;
        }

        // Everything else — `event_msg` and `world_state` — is Codex's own
        // state rather than transcript. `event_msg` in particular mirrors the
        // `response_item` stream for the UI, so keeping both would archive the
        // same conversation twice over.
    }

    // A session must have a start, and the archive files by it. Deriving it
    // from the rollout's own first record is derivation; picking "now" would be
    // invention.
    let started_at = earliest.ok_or_else(|| AdapterError::Empty {
        provider: provider.clone(),
        provider_session_id: provider_session_id.to_string(),
    })?;

    let mut session = Session::new(provider.clone(), provider_session_id, started_at);
    session.id = SessionId::derive(provider, provider_session_id);
    session.model = model;
    session.ended_at = latest.filter(|l| *l != started_at);
    session.project = cwd.or(project_hint);
    // Codex records no branch or commit. Absent, not guessed.
    session.git = None;
    session.events = events;

    if session.events.is_empty() {
        return Err(AdapterError::Empty {
            provider: provider.clone(),
            provider_session_id: provider_session_id.to_string(),
        });
    }

    Ok(session)
}

/// The events one transcript item contributes, in order.
fn item_events(item: &Item, at: Option<OffsetDateTime>, events: &mut Vec<SessionEvent>) {
    let known = match item {
        Item::Known(known) => known,
        // An item this adapter cannot name is kept rather than dropped, so a
        // new Codex release cannot quietly cost the user part of a
        // conversation.
        Item::Unknown(value) => {
            events.push(SessionEvent::Other {
                at,
                provider_kind: item_type_of(value),
                content: value.to_string(),
            });
            return;
        }
    };

    match known {
        KnownItem::Message { role, content } => {
            let Some(content) = content else { return };
            for piece in content {
                let Some(text) = piece.text() else { continue };
                events.push(message_event(role.as_deref(), at, text.to_string()));
            }
        }

        KnownItem::FunctionCall {
            name,
            arguments,
            call_id,
        } => {
            let name = name.clone().unwrap_or_default();

            // A shell command is worth recording as one. The tool call is kept
            // too: the call is what the model asked for, the command is what
            // ran, and they are not the same claim.
            if name == EXEC_COMMAND_TOOL {
                if let Some(parsed) = arguments.as_deref().and_then(ExecCommandArguments::parse) {
                    if let Some(cmd) = parsed.cmd.filter(|c| !c.is_empty()) {
                        events.push(SessionEvent::Command {
                            at,
                            command: cmd,
                            // Codex records the outcome in the tool's output,
                            // not as a status. Absent rather than assumed to
                            // be success.
                            exit_code: None,
                            output: None,
                        });
                    }
                }
            }

            events.push(SessionEvent::ToolCall {
                at,
                name,
                // Verbatim, as recorded.
                arguments: arguments.clone(),
                call_id: call_id.clone(),
            });
        }

        KnownItem::CustomToolCall {
            name,
            input,
            call_id,
        } => events.push(SessionEvent::ToolCall {
            at,
            name: name.clone().unwrap_or_default(),
            arguments: input.as_ref().map(ToString::to_string),
            call_id: call_id.clone(),
        }),

        KnownItem::FunctionCallOutput { call_id, output }
        | KnownItem::CustomToolCallOutput { call_id, output } => {
            events.push(SessionEvent::ToolResult {
                at,
                call_id: call_id.clone(),
                content: output.as_ref().map(flatten_value).unwrap_or_default(),
                // Codex does not distinguish success from failure on the
                // record. `None` says so; `Some(false)` would claim otherwise.
                failed: None,
            })
        }

        KnownItem::Reasoning { summary, content } => {
            // Recall has no variant for model reasoning. Keeping it as Other
            // preserves it; dropping it would lose the thinking someone came
            // back for. `encrypted_content` is not kept — it is opaque, and
            // nobody can read it back.
            for piece in summary.iter().chain(content.iter()).flatten() {
                let Some(text) = piece.text() else { continue };
                events.push(SessionEvent::Other {
                    at,
                    provider_kind: "reasoning".to_string(),
                    content: text.to_string(),
                });
            }
        }
    }
}

/// The event a message becomes, by who said it.
///
/// Codex has a third role, `developer`, which is neither the person nor the
/// model — it carries instructions injected into the conversation. It is kept
/// as `Other` rather than being filed under one of the two, which would
/// misattribute it.
fn message_event(role: Option<&str>, at: Option<OffsetDateTime>, content: String) -> SessionEvent {
    match role {
        Some("assistant") => SessionEvent::AssistantMessage { at, content },
        Some("user") => SessionEvent::UserMessage { at, content },
        other => SessionEvent::Other {
            at,
            provider_kind: other.unwrap_or("message").to_string(),
            content,
        },
    }
}

/// A tool result's body, which may be text or structured.
///
/// Structured output is kept as the JSON it was recorded as, rather than being
/// flattened into prose that no longer says what the tool returned.
fn flatten_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => {
            // A list of content pieces, when that is what it is; otherwise the
            // raw JSON, so nothing is lost.
            let text: Vec<String> = items
                .iter()
                .filter_map(|item| {
                    serde_json::from_value::<ContentItem>(item.clone())
                        .ok()
                        .and_then(|c| c.text().map(str::to_string))
                })
                .collect();
            if text.is_empty() {
                value.to_string()
            } else {
                text.join("\n")
            }
        }
        other => other.to_string(),
    }
}

/// What Codex called an item this adapter does not recognise.
fn item_type_of(value: &serde_json::Value) -> String {
    value
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("response_item")
        .to_string()
}

/// Codex writes RFC 3339 timestamps with a `Z` offset.
///
/// A timestamp that cannot be read is treated as absent rather than replaced.
fn parse_timestamp(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::parse::parse_file;

    fn provider() -> Provider {
        Provider::new("codex").expect("provider")
    }

    fn session_from(lines: &str) -> Session {
        try_session_from(lines).expect("normalize")
    }

    fn try_session_from(lines: &str) -> Result<Session, AdapterError> {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, lines).expect("write");
        let parsed = parse_file(&provider(), "s1", &path).expect("parse");
        normalize(&provider(), "s1", None, &parsed)
    }

    /// A `session_meta` line, as Codex writes it first in every rollout.
    fn meta(at: &str) -> String {
        format!(
            r#"{{"timestamp":"{at}","type":"session_meta","payload":{{"session_id":"s1","cwd":"/w/project","cli_version":"0.152.1"}}}}"#
        )
    }

    fn message(at: &str, role: &str, kind: &str, text: &str) -> String {
        format!(
            r#"{{"timestamp":"{at}","type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"{kind}","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn a_conversation_becomes_the_events_it_was() {
        let session = session_from(&format!(
            "{}\n{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:10:01Z", "user", "input_text", "add a test"),
            message("2026-08-03T13:10:02Z", "assistant", "output_text", "done"),
        ));

        let kinds: Vec<_> = session.events.iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, ["user", "assistant"]);
        assert_eq!(session.events[0].searchable_text(), Some("add a test"));
        assert_eq!(session.events[1].searchable_text(), Some("done"));
    }

    #[test]
    fn the_session_starts_when_its_first_record_did() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:45:00Z", "user", "input_text", "hi"),
        ));
        assert_eq!(
            session.started_at,
            OffsetDateTime::parse("2026-08-03T13:10:00Z", &Rfc3339).expect("parse")
        );
        assert_eq!(
            session.ended_at,
            Some(OffsetDateTime::parse("2026-08-03T13:45:00Z", &Rfc3339).expect("parse"))
        );
    }

    #[test]
    fn the_model_comes_from_the_turn_context() {
        // Codex records the model per turn, not on the messages.
        let session = session_from(&format!(
            "{}\n{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/w/project"}}"#,
            message("2026-08-03T13:10:02Z", "user", "input_text", "hi"),
        ));
        assert_eq!(session.model.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn the_model_a_session_started_under_is_the_one_it_reports() {
        // A review pass later in the session runs under a different model.
        // Letting it overwrite would misreport what the session ran as.
        let session = session_from(&format!(
            "{}\n{}\n{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            message("2026-08-03T13:10:02Z", "user", "input_text", "hi"),
            r#"{"timestamp":"2026-08-03T13:10:03Z","type":"turn_context","payload":{"model":"codex-auto-review"}}"#,
        ));
        assert_eq!(session.model.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn a_shell_command_is_recorded_as_the_command_it_was() {
        // The thing Claude Code cannot supply: Codex records the command line
        // itself, so "what did this agent run against my machine" is
        // answerable directly.
        let session = session_from(&format!(
            "{}\n{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test --workspace\",\"workdir\":\"/w\"}","call_id":"c1"}}"#,
            r#"{"timestamp":"2026-08-03T13:10:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"test result: ok"}}"#,
        ));

        let kinds: Vec<_> = session.events.iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, ["command", "tool_call", "tool_result"]);

        let SessionEvent::Command {
            command, exit_code, ..
        } = &session.events[0]
        else {
            panic!("expected a command");
        };
        assert_eq!(command, "cargo test --workspace");
        assert_eq!(*exit_code, None, "an exit code Codex never recorded");
    }

    #[test]
    fn a_tool_call_keeps_its_arguments_verbatim() {
        let arguments = r#"{"cmd":"cargo test --workspace","workdir":"/w"}"#;
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test --workspace\",\"workdir\":\"/w\"}","call_id":"c1"}}"#,
        ));

        let SessionEvent::ToolCall {
            name,
            arguments: kept,
            call_id,
            ..
        } = &session.events[1]
        else {
            panic!("expected a tool call");
        };
        assert_eq!(name, "exec_command");
        assert_eq!(kept.as_deref(), Some(arguments));
        assert_eq!(call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn a_tool_call_survives_arguments_that_cannot_be_read() {
        // Only the derived command is lost; the call itself is still recorded.
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"not json","call_id":"c1"}}"#,
        ));
        let kinds: Vec<_> = session.events.iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, ["tool_call"]);
    }

    #[test]
    fn a_tool_result_does_not_claim_to_know_whether_it_failed() {
        // Codex records no status. Some(false) would claim success it never
        // stated.
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"whatever"}}"#,
        ));
        let SessionEvent::ToolResult { failed, .. } = &session.events[0] else {
            panic!("expected a tool result");
        };
        assert_eq!(*failed, None);
    }

    #[test]
    fn a_long_tool_result_is_not_truncated() {
        let huge = "x".repeat(100_000);
        let output = format!(
            r#"{{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"c1","output":"{huge}"}}}}"#
        );
        let session = session_from(&format!("{}\n{}\n", meta("2026-08-03T13:10:00Z"), output,));
        let SessionEvent::ToolResult { content, .. } = &session.events[0] else {
            panic!("expected a tool result");
        };
        assert_eq!(content.len(), huge.len());
    }

    #[test]
    fn structured_tool_output_is_kept_as_what_it_was() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":{"page":1,"thread":"t"}}}"#,
        ));
        let SessionEvent::ToolResult { content, .. } = &session.events[0] else {
            panic!("expected a tool result");
        };
        assert!(
            content.contains("\"page\""),
            "structured output was lost: {content}"
        );
    }

    #[test]
    fn reasoning_is_preserved_rather_than_dropped() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"weighing two options"}],"encrypted_content":"opaque"}}"#,
        ));
        let SessionEvent::Other {
            provider_kind,
            content,
            ..
        } = &session.events[0]
        else {
            panic!("expected reasoning");
        };
        assert_eq!(provider_kind, "reasoning");
        assert_eq!(content, "weighing two options");
        assert!(
            !session
                .events
                .iter()
                .any(|e| e.searchable_text() == Some("opaque")),
            "opaque encrypted content was archived"
        );
    }

    #[test]
    fn a_developer_message_is_not_filed_as_the_person_or_the_model() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message(
                "2026-08-03T13:10:01Z",
                "developer",
                "input_text",
                "injected"
            ),
        ));
        let SessionEvent::Other { provider_kind, .. } = &session.events[0] else {
            panic!("expected other, got {:?}", session.events[0].kind());
        };
        assert_eq!(provider_kind, "developer");
    }

    #[test]
    fn an_unrecognised_item_is_kept_under_the_name_codex_gave_it() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            r#"{"timestamp":"2026-08-03T13:10:01Z","type":"response_item","payload":{"type":"web_search_call","status":"completed"}}"#,
        ));
        let SessionEvent::Other { provider_kind, .. } = &session.events[0] else {
            panic!("an unknown item was dropped");
        };
        assert_eq!(provider_kind, "web_search_call");
    }

    #[test]
    fn the_system_prompt_is_kept() {
        let session = session_from(&format!(
            "{}\n{}\n",
            r#"{"timestamp":"2026-08-03T13:10:00Z","type":"session_meta","payload":{"session_id":"s1","cwd":"/w","base_instructions":{"text":"be pragmatic"}}}"#,
            message("2026-08-03T13:10:01Z", "user", "input_text", "hi"),
        ));
        assert_eq!(session.events[0].searchable_text(), Some("be pragmatic"));
    }

    #[test]
    fn codexs_own_interface_events_are_not_archived_twice() {
        // event_msg mirrors the response_item stream for the UI. Keeping both
        // would archive the conversation twice over.
        let session = session_from(&format!(
            "{}\n{}\n{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:10:01Z", "assistant", "output_text", "done"),
            r#"{"timestamp":"2026-08-03T13:10:02Z","type":"event_msg","payload":{"type":"item_completed","item":{"text":"done"}}}"#,
            r#"{"timestamp":"2026-08-03T13:10:03Z","type":"world_state","payload":{"full":true}}"#,
        ));
        let assistant = session
            .events
            .iter()
            .filter(|e| e.kind() == "assistant")
            .count();
        assert_eq!(assistant, 1, "the same message was archived twice");
    }

    #[test]
    fn the_project_is_the_recorded_working_directory() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:10:01Z", "user", "input_text", "hi"),
        ));
        assert_eq!(session.project, Some(PathBuf::from("/w/project")));
    }

    #[test]
    fn a_codex_session_claims_no_git_context() {
        // Codex records no branch and no commit. #32 derives those from the
        // repository; claiming one here would be invention.
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:10:01Z", "user", "input_text", "hi"),
        ));
        assert_eq!(session.git, None);
    }

    #[test]
    fn a_rollout_with_no_timestamps_cannot_be_archived() {
        // The archive files by start date. Substituting "now" would file the
        // session under the day it was synced rather than the day it happened.
        let err = try_session_from(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        )
        .expect_err("should be empty");
        assert!(matches!(err, AdapterError::Empty { .. }), "{err:?}");
    }

    #[test]
    fn a_rollout_of_pure_bookkeeping_holds_no_session() {
        let err = try_session_from(
            r#"{"timestamp":"2026-08-03T13:10:00Z","type":"world_state","payload":{"full":true}}"#,
        )
        .expect_err("should be empty");
        assert!(matches!(err, AdapterError::Empty { .. }), "{err:?}");
    }

    #[test]
    fn events_keep_the_order_the_rollout_recorded_them_in() {
        let session = session_from(&format!(
            "{}\n{}\n{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:10:01Z", "user", "input_text", "first"),
            message("2026-08-03T13:10:02Z", "assistant", "output_text", "second"),
            message("2026-08-03T13:10:03Z", "user", "input_text", "third"),
        ));
        let text: Vec<_> = session
            .events
            .iter()
            .filter_map(|e| e.searchable_text())
            .collect();
        assert_eq!(text, ["first", "second", "third"]);
    }

    #[test]
    fn the_derived_id_matches_the_provider_fields_it_came_from() {
        let session = session_from(&format!(
            "{}\n{}\n",
            meta("2026-08-03T13:10:00Z"),
            message("2026-08-03T13:10:01Z", "user", "input_text", "hi"),
        ));
        assert_eq!(
            session.id,
            SessionId::derive(&provider(), &session.provider_session_id)
        );
    }
}
