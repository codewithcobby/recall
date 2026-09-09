//! Turning Claude Code's records into Recall's model.
//!
//! The rule throughout is the one from `.github/CONTRIBUTING.md`: preserve what
//! is there, derive what follows from it, invent nothing. A field Claude Code
//! does not record stays `None`.

use std::path::PathBuf;

use recall_core::{
    AdapterError, FileAction, GitContext, Provider, Session, SessionEvent, SessionId,
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::parse::ParsedFile;
use super::record::{Block, Content, KnownBlock, Record, ToolUseResult};

/// Build a session from the files that make it up.
///
/// `files` is the session's own transcript followed by any sub-agent
/// transcripts. They are merged into one chronology, because that is what
/// happened: a sub-agent runs *during* the session, not after it.
pub fn normalize(
    provider: &Provider,
    provider_session_id: &str,
    project_hint: Option<PathBuf>,
    files: &[ParsedFile],
) -> Result<Session, AdapterError> {
    let mut timed: Vec<(Option<OffsetDateTime>, usize, SessionEvent)> = Vec::new();
    let mut model = None;
    let mut cwd = None;
    let mut branch = None;
    let mut earliest: Option<OffsetDateTime> = None;
    let mut latest: Option<OffsetDateTime> = None;
    let mut order = 0usize;

    for file in files {
        for record in &file.records {
            let at = record.timestamp.as_deref().and_then(parse_timestamp);
            if let Some(at) = at {
                earliest = Some(earliest.map_or(at, |e| e.min(at)));
                latest = Some(latest.map_or(at, |l| l.max(at)));
            }

            // The cwd inside the session is exact, unlike the path decoded from
            // the project directory's name.
            if cwd.is_none() {
                if let Some(c) = record.cwd.as_deref().filter(|c| !c.is_empty()) {
                    cwd = Some(PathBuf::from(c));
                }
            }
            if branch.is_none() {
                if let Some(b) = record.git_branch.as_deref().filter(|b| is_branch_name(b)) {
                    branch = Some(b.to_string());
                }
            }
            // First real model wins. Claude Code writes placeholders in this
            // field for messages it generated itself, and a placeholder
            // arriving after a real identifier would otherwise overwrite it.
            if model.is_none() {
                if let Some(m) = record
                    .message
                    .as_ref()
                    .and_then(|m| m.model.as_deref())
                    .filter(|m| is_real_model(m))
                {
                    model = Some(m.to_string());
                }
            }

            for event in events_from(record, at) {
                timed.push((at, order, event));
                order += 1;
            }
        }
    }

    // Chronological, with file order settling ties and records that carry no
    // timestamp staying where they were found.
    timed.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y).then(a.1.cmp(&b.1)),
        _ => a.1.cmp(&b.1),
    });

    // A session must have a start, and the archive files by it. Deriving it
    // from the session's own first record is derivation; picking "now" would be
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
    session.git = branch.map(|b| GitContext {
        branch: Some(b),
        // Claude Code records the branch but not the commit. #31 derives those
        // from the repository; guessing them here would be invention.
        ..GitContext::default()
    });
    session.events = timed.into_iter().map(|(_, _, e)| e).collect();

    if session.events.is_empty() {
        return Err(AdapterError::Empty {
            provider: provider.clone(),
            provider_session_id: provider_session_id.to_string(),
        });
    }

    Ok(session)
}

/// The events one record contributes, in order.
fn events_from(record: &Record, at: Option<OffsetDateTime>) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    let kind = record.kind.as_deref().unwrap_or_default();

    match kind {
        "user" | "assistant" => {
            if let Some(content) = record.message.as_ref().and_then(|m| m.content.as_ref()) {
                content_events(content, kind, at, &mut events);
            }
            // What the tool actually did, which the call itself does not say.
            if let Some(result) = record.tool_use_result.as_ref() {
                tool_result_events(result, at, &mut events);
            }
        }
        "system" => {
            if let Some(text) = record.content.as_ref().and_then(value_as_text) {
                events.push(SessionEvent::Other {
                    at,
                    provider_kind: record
                        .subtype
                        .clone()
                        .unwrap_or_else(|| "system".to_string()),
                    content: text,
                });
            }
        }
        "attachment" => {
            if let Some(attachment) = record.attachment.as_ref() {
                // Claude Code's own reminders are bookkeeping, in the same
                // category as the record types skipped below. Real attachments
                // are what the session was actually shown, and are kept.
                if !attachment.is_internal() {
                    if let Some(content) = attachment.text() {
                        events.push(SessionEvent::Other {
                            at,
                            provider_kind: attachment.label(),
                            content,
                        });
                    }
                }
            }
        }
        // Everything else is Claude Code's own state rather than transcript.
        _ => {}
    }

    events
}

/// Events from a message body.
fn content_events(
    content: &Content,
    role: &str,
    at: Option<OffsetDateTime>,
    events: &mut Vec<SessionEvent>,
) {
    match content {
        Content::Text(text) => events.push(message_event(role, at, text.clone())),
        Content::Blocks(blocks) => {
            for block in blocks {
                match block {
                    Block::Known(KnownBlock::Text { text }) => {
                        events.push(message_event(role, at, text.clone()));
                    }
                    Block::Known(KnownBlock::Thinking { thinking }) => {
                        // Recall has no variant for extended thinking. Keeping
                        // it as Other preserves it; dropping it would lose the
                        // reasoning someone came back for.
                        events.push(SessionEvent::Other {
                            at,
                            provider_kind: "thinking".to_string(),
                            content: thinking.clone(),
                        });
                    }
                    Block::Known(KnownBlock::ToolUse { id, name, input }) => {
                        events.push(SessionEvent::ToolCall {
                            at,
                            name: name.clone(),
                            // Verbatim, as recorded.
                            arguments: input.as_ref().map(ToString::to_string),
                            call_id: id.clone(),
                        });
                    }
                    Block::Known(KnownBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    }) => {
                        events.push(SessionEvent::ToolResult {
                            at,
                            call_id: tool_use_id.clone(),
                            content: content.as_ref().map(flatten_content).unwrap_or_default(),
                            failed: *is_error,
                        });
                    }
                    // An unrecognised block is kept rather than dropped, so a
                    // new Claude Code release cannot quietly cost the user
                    // part of a conversation.
                    Block::Unknown(value) => events.push(SessionEvent::Other {
                        at,
                        provider_kind: block_type_of(value),
                        content: value.to_string(),
                    }),
                }
            }
        }
    }
}

/// Events derived from what a tool actually did.
fn tool_result_events(
    result: &ToolUseResult,
    at: Option<OffsetDateTime>,
    events: &mut Vec<SessionEvent>,
) {
    if let Some(text) = result.as_text() {
        // A rejection or an error, recorded as plain text rather than an object.
        events.push(SessionEvent::ToolResult {
            at,
            call_id: None,
            content: text.to_string(),
            failed: Some(true),
        });
        return;
    }

    let Some(structured) = result.structured() else {
        return;
    };

    if structured.is_command() {
        let output = match (&structured.stdout, &structured.stderr) {
            (Some(out), Some(err)) if !err.is_empty() => Some(format!("{out}{err}")),
            (Some(out), _) => Some(out.clone()),
            (None, Some(err)) => Some(err.clone()),
            (None, None) => None,
        };
        events.push(SessionEvent::Command {
            at,
            // Claude Code records what the command produced, not the command
            // itself; the invocation is in the tool call's arguments.
            command: String::new(),
            // Not recorded. Absent rather than assumed to be success.
            exit_code: None,
            output,
        });
    }

    if let Some(path) = structured.touched_file() {
        events.push(SessionEvent::FileChange {
            at,
            action: if structured.changed_the_file() {
                FileAction::Modified
            } else {
                FileAction::Read
            },
            // Data, never a path Recall opens.
            path: PathBuf::from(path),
        });
    }
}

fn message_event(role: &str, at: Option<OffsetDateTime>, content: String) -> SessionEvent {
    if role == "assistant" {
        SessionEvent::AssistantMessage { at, content }
    } else {
        SessionEvent::UserMessage { at, content }
    }
}

/// A tool result's content, which is string-or-array like a message body.
fn flatten_content(content: &Content) -> String {
    match content {
        Content::Text(text) => text.clone(),
        Content::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                Block::Known(KnownBlock::Text { text }) => text.clone(),
                Block::Known(KnownBlock::Thinking { thinking }) => thinking.clone(),
                Block::Known(_) => String::new(),
                Block::Unknown(v) => v.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The `type` of an unrecognised block, so it can be labelled honestly.
fn block_type_of(value: &serde_json::Value) -> String {
    value
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// A `system` record's content, which may be a string or a structure.
fn value_as_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) if s.is_empty() => None,
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Whether this names a branch rather than the absence of one.
///
/// Claude Code records `HEAD` when the repository is on a detached HEAD —
/// mid-rebase, on a checked-out tag, in some worktree states. It is not a
/// branch name, and storing it as one collapses every detached session across
/// every project into a single meaningless group, which breaks the question the
/// README promises to answer: *which AI session worked on this branch?*
///
/// The commit is what would identify such a session, and Claude Code does not
/// record it. So the honest answer is that the branch is unknown. #31 derives
/// git state from the repository itself and should treat detached HEAD the same
/// way.
fn is_branch_name(branch: &str) -> bool {
    !branch.is_empty() && branch != "HEAD"
}

/// Whether this names a model rather than marking the absence of one.
///
/// Claude Code writes `<synthetic>` on messages it produced itself — an
/// interrupted turn, an injected notice — rather than obtained from the API.
/// Archiving that as the session's model stores something that looks like a
/// value and matches no model that exists, which is worse than absence:
/// listings, the index (#34) and search would all carry it.
///
/// The angle brackets are the tell, so treating the shape as the marker rather
/// than one exact string handles a new placeholder without a code change.
fn is_real_model(model: &str) -> bool {
    !model.is_empty() && !(model.starts_with('<') && model.ends_with('>'))
}

/// Claude Code writes ISO-8601 with a `Z`.
fn parse_timestamp(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::parse::parse_file;

    fn provider() -> Provider {
        Provider::new("claude-code").expect("provider")
    }

    /// Normalize some JSON Lines as though they were one session file.
    fn session_from(lines: &str) -> Session {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, lines).expect("write");
        let parsed = parse_file(&provider(), "sess-1", &path).expect("parse");
        normalize(&provider(), "sess-1", None, &[parsed]).expect("normalize")
    }

    const AT: &str = "2026-09-08T12:00:00.000Z";

    #[test]
    fn metadata_comes_from_the_records() {
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","cwd":"/w/project","gitBranch":"dev","sessionId":"sess-1","version":"2.1.228","message":{{"role":"user","content":"hello"}}}}
{{"type":"assistant","timestamp":"2026-09-08T13:30:00.000Z","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"hi"}}]}}}}"#
        ));

        assert_eq!(s.provider.as_str(), "claude-code");
        assert_eq!(s.provider_session_id, "sess-1");
        assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(s.project, Some(PathBuf::from("/w/project")));
        assert_eq!(
            s.git.as_ref().and_then(|g| g.branch.as_deref()),
            Some("dev")
        );
        assert_eq!(s.duration().map(|d| d.whole_minutes()), Some(90));
    }

    #[test]
    fn a_synthetic_model_placeholder_is_not_a_model() {
        // Claude Code writes this for messages it generated itself. Archiving
        // it would store a value matching no model that exists.
        let s = session_from(&format!(
            r#"{{"type":"assistant","timestamp":"{AT}","message":{{"model":"<synthetic>","content":[{{"type":"text","text":"x"}}]}}}}"#
        ));
        assert_eq!(s.model, None);
    }

    #[test]
    fn a_real_model_is_not_overwritten_by_a_later_placeholder() {
        // The exact ordering that produced the bug.
        let s = session_from(&format!(
            r#"{{"type":"assistant","timestamp":"{AT}","message":{{"model":"claude-opus-5","content":[{{"type":"text","text":"a"}}]}}}}
{{"type":"assistant","timestamp":"{AT}","message":{{"model":"<synthetic>","content":[{{"type":"text","text":"b"}}]}}}}"#
        ));
        assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn a_placeholder_before_a_real_model_does_not_win_either() {
        let s = session_from(&format!(
            r#"{{"type":"assistant","timestamp":"{AT}","message":{{"model":"<synthetic>","content":[{{"type":"text","text":"a"}}]}}}}
{{"type":"assistant","timestamp":"{AT}","message":{{"model":"claude-opus-5","content":[{{"type":"text","text":"b"}}]}}}}"#
        ));
        assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn any_bracketed_placeholder_is_treated_as_absence() {
        for placeholder in ["<synthetic>", "<none>", "<unknown>", ""] {
            let s = session_from(&format!(
                r#"{{"type":"assistant","timestamp":"{AT}","message":{{"model":"{placeholder}","content":[{{"type":"text","text":"x"}}]}}}}"#
            ));
            assert_eq!(s.model, None, "{placeholder:?} was archived as a model");
        }
    }

    #[test]
    fn a_detached_head_is_not_a_branch() {
        // Six of the eleven sessions in the archive that exposed this recorded
        // "HEAD". Storing it collapses every detached session everywhere into
        // one group that answers no useful question.
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","gitBranch":"HEAD","message":{{"content":"x"}}}}"#
        ));
        assert_eq!(s.git.and_then(|g| g.branch), None);
    }

    #[test]
    fn a_real_branch_after_a_detached_head_is_still_found() {
        // A session that starts detached and ends on a branch - a rebase
        // finishing, say - should record the branch.
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","gitBranch":"HEAD","message":{{"content":"a"}}}}
{{"type":"user","timestamp":"{AT}","gitBranch":"fix/312-vendor-order","message":{{"content":"b"}}}}"#
        ));
        assert_eq!(
            s.git.and_then(|g| g.branch).as_deref(),
            Some("fix/312-vendor-order")
        );
    }

    #[test]
    fn a_branch_that_merely_contains_head_is_kept() {
        // "HEAD" exactly is the marker. A branch called head-refactor is a
        // branch.
        for name in ["head-refactor", "feature/HEADer", "HEADS"] {
            let s = session_from(&format!(
                r#"{{"type":"user","timestamp":"{AT}","gitBranch":"{name}","message":{{"content":"x"}}}}"#
            ));
            assert_eq!(
                s.git.and_then(|g| g.branch).as_deref(),
                Some(name),
                "{name} was discarded"
            );
        }
    }

    #[test]
    fn the_commit_is_left_empty_because_claude_does_not_record_it() {
        // Guessing it would be invention. #31 derives it from the repository.
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","gitBranch":"dev","message":{{"content":"x"}}}}"#
        ));
        let git = s.git.expect("git context");
        assert_eq!(git.branch.as_deref(), Some("dev"));
        assert_eq!(git.commit_at_start, None);
        assert_eq!(git.commit_at_end, None);
    }

    #[test]
    fn messages_tool_calls_and_results_become_events() {
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","message":{{"role":"user","content":"refactor it"}}}}
{{"type":"assistant","timestamp":"{AT}","message":{{"role":"assistant","content":[{{"type":"text","text":"reading first"}},{{"type":"tool_use","id":"t1","name":"read_file","input":{{"path":"a.rs"}}}}]}}}}
{{"type":"user","timestamp":"{AT}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"fn main(){{}}","is_error":false}}]}}}}"#
        ));

        let kinds: Vec<_> = s.events.iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds,
            ["user", "assistant", "tool_call", "tool_result"],
            "got {kinds:?}"
        );

        let SessionEvent::ToolCall {
            name,
            arguments,
            call_id,
            ..
        } = &s.events[2]
        else {
            panic!("expected a tool call");
        };
        assert_eq!(name, "read_file");
        assert_eq!(call_id.as_deref(), Some("t1"));
        assert!(
            arguments.as_deref().unwrap_or_default().contains("a.rs"),
            "arguments were not preserved verbatim"
        );
    }

    #[test]
    fn thinking_is_preserved_rather_than_dropped() {
        // Recall has no variant for extended thinking, and "what was it
        // reasoning about" is exactly what someone comes back for.
        let s = session_from(&format!(
            r#"{{"type":"assistant","timestamp":"{AT}","message":{{"content":[{{"type":"thinking","thinking":"weighing two designs","signature":"sig"}}]}}}}"#
        ));
        let SessionEvent::Other {
            provider_kind,
            content,
            ..
        } = &s.events[0]
        else {
            panic!("expected an other event");
        };
        assert_eq!(provider_kind, "thinking");
        assert_eq!(content, "weighing two designs");
    }

    #[test]
    fn commands_and_file_changes_are_derived_from_the_tool_result() {
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","toolUseResult":{{"stdout":"42 passed","stderr":"","interrupted":false}}}}
{{"type":"user","timestamp":"{AT}","toolUseResult":{{"filePath":"/w/a.rs","structuredPatch":[],"originalFile":"old"}}}}
{{"type":"user","timestamp":"{AT}","toolUseResult":{{"filePath":"/w/b.rs"}}}}"#
        ));
        let kinds: Vec<_> = s.events.iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, ["command", "file_change", "file_change"]);

        let SessionEvent::FileChange { action, path, .. } = &s.events[1] else {
            panic!("expected a file change");
        };
        assert_eq!(*action, FileAction::Modified);
        assert_eq!(path, &PathBuf::from("/w/a.rs"));

        // Read, not modified: nothing says it changed.
        let SessionEvent::FileChange { action, .. } = &s.events[2] else {
            panic!("expected a file change");
        };
        assert_eq!(*action, FileAction::Read);
    }

    #[test]
    fn an_exit_code_that_was_not_recorded_stays_absent() {
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","toolUseResult":{{"stdout":"ok"}}}}"#
        ));
        let SessionEvent::Command { exit_code, .. } = &s.events[0] else {
            panic!("expected a command");
        };
        assert_eq!(*exit_code, None, "an exit code was invented");
    }

    #[test]
    fn a_rejected_tool_is_kept_as_a_failed_result() {
        let s = session_from(&format!(
            r#"{{"type":"user","timestamp":"{AT}","toolUseResult":"User rejected tool use"}}"#
        ));
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
    fn claude_codes_own_bookkeeping_is_not_archived() {
        // 17 of the 21 record types are Claude Code's internal state. Archiving
        // them would bury the conversation.
        let s = session_from(&format!(
            r#"{{"type":"ai-title","aiTitle":"x","sessionId":"s"}}
{{"type":"mode","mode":"normal","sessionId":"s"}}
{{"type":"worktree-state","worktreeSession":{{}},"sessionId":"s"}}
{{"type":"user","timestamp":"{AT}","message":{{"content":"the only real message"}}}}"#
        ));
        assert_eq!(s.event_count(), 1);
        assert_eq!(s.events[0].kind(), "user");
    }

    #[test]
    fn internal_reminders_are_not_archived_but_real_attachments_are() {
        let s = session_from(&format!(
            r#"{{"type":"attachment","timestamp":"{AT}","attachment":{{"type":"total_tokens_reminder","text":"you have used N tokens"}}}}
{{"type":"attachment","timestamp":"{AT}","attachment":{{"type":"edited_text_file","filename":"a.rs","snippet":"fn main() {{}}"}}}}"#
        ));
        assert_eq!(s.event_count(), 1, "a reminder was archived");
        let SessionEvent::Other {
            provider_kind,
            content,
            ..
        } = &s.events[0]
        else {
            panic!("expected an other event");
        };
        assert_eq!(provider_kind, "attachment:edited_text_file:a.rs");
        assert_eq!(content, "fn main() {}");
    }

    #[test]
    fn an_unknown_attachment_type_is_kept() {
        // The deny-list direction matters: a new attachment type is preserved
        // rather than silently lost.
        let s = session_from(&format!(
            r#"{{"type":"attachment","timestamp":"{AT}","attachment":{{"type":"something_new","text":"content worth keeping"}}}}"#
        ));
        assert_eq!(s.event_count(), 1);
    }

    #[test]
    fn sub_agent_work_is_merged_into_one_chronology() {
        // A sub-agent runs during the session, not after it, so its events
        // belong in order rather than appended.
        let dir = tempfile::tempdir().expect("temp dir");
        let main = dir.path().join("main.jsonl");
        let sub = dir.path().join("sub.jsonl");
        std::fs::write(
            &main,
            "{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"message\":{\"content\":\"first\"}}\n\
             {\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:30.000Z\",\"message\":{\"content\":\"third\"}}\n",
        )
        .expect("write");
        std::fs::write(
            &sub,
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-08T12:00:15.000Z\",\"isSidechain\":true,\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}}\n",
        )
        .expect("write");

        let files = vec![
            parse_file(&provider(), "s", &main).expect("parse main"),
            parse_file(&provider(), "s", &sub).expect("parse sub"),
        ];
        let s = normalize(&provider(), "s", None, &files).expect("normalize");

        let text: Vec<_> = s
            .events
            .iter()
            .filter_map(|e| e.searchable_text())
            .collect();
        assert_eq!(
            text,
            ["first", "second", "third"],
            "sub-agent work was appended rather than interleaved"
        );
    }

    #[test]
    fn a_session_with_no_timestamped_records_is_refused() {
        // The archive files by start date, so a session without one cannot be
        // stored. Inventing "now" would put it under the wrong day forever.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "{\"type\":\"ai-title\",\"aiTitle\":\"x\"}\n").expect("write");
        let parsed = parse_file(&provider(), "s", &path).expect("parse");
        assert!(matches!(
            normalize(&provider(), "s", None, &[parsed]),
            Err(AdapterError::Empty { .. })
        ));
    }

    #[test]
    fn the_recorded_cwd_wins_over_the_directory_name_hint() {
        // The slug loses information; the cwd inside the session is exact.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"user\",\"timestamp\":\"{AT}\",\"cwd\":\"/w/my-project\",\"message\":{{\"content\":\"x\"}}}}\n"
            ),
        )
        .expect("write");
        let parsed = parse_file(&provider(), "s", &path).expect("parse");
        let s = normalize(
            &provider(),
            "s",
            Some(PathBuf::from("/w/my/project")),
            &[parsed],
        )
        .expect("normalize");
        assert_eq!(s.project, Some(PathBuf::from("/w/my-project")));
    }
}
