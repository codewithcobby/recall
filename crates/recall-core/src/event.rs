//! What happened inside a session.
//!
//! A transcript is only worth archiving if it survives intact. These variants
//! exist so that tool calls, their results, the commands an agent ran, and the
//! files it touched are all still there when someone reads the session back —
//! not summarized, not flattened into prose.
//!
//! Adapters map a provider's records onto these. Anything a provider does not
//! record stays `None`; nothing here is inferred.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// What an agent did to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    /// The file was read into the session.
    Read,
    /// The file did not exist before.
    Created,
    /// The file existed and its contents changed.
    Modified,
    /// The file was removed.
    Deleted,
}

impl FileAction {
    /// The action as a lowercase word, for output and for the wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            FileAction::Read => "read",
            FileAction::Created => "created",
            FileAction::Modified => "modified",
            FileAction::Deleted => "deleted",
        }
    }
}

/// One thing that happened during a session, in the order it happened.
///
/// Timestamps are optional throughout: plenty of formats record ordering
/// without recording a clock, and an invented timestamp is worse than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Tagged by kind, so a line in the archive says what it is before it says
// anything else, and an unknown tag fails loudly instead of silently matching
// the wrong variant.
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Something the person said to the agent.
    #[serde(rename = "user")]
    UserMessage {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        content: String,
    },

    /// Something the agent said back.
    #[serde(rename = "assistant")]
    AssistantMessage {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        content: String,
    },

    /// The agent invoked a tool.
    ToolCall {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        /// The tool's name as the provider recorded it.
        name: String,
        /// Arguments exactly as recorded, verbatim.
        ///
        /// Deliberately text rather than parsed JSON. Storing it verbatim keeps
        /// the core free of any assumption about a provider's encoding, and
        /// keeps the value lossless for a reader who needs to see precisely
        /// what the agent asked for.
        arguments: Option<String>,
        /// Correlates with the [`SessionEvent::ToolResult`] that answered it,
        /// when the provider records such a link.
        call_id: Option<String>,
    },

    /// What a tool returned.
    ToolResult {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        /// Links back to the [`SessionEvent::ToolCall`], when available.
        call_id: Option<String>,
        /// The result verbatim. Not truncated, however long or noisy.
        content: String,
        /// Whether the tool reported failure. `None` when the format does not
        /// distinguish success from failure.
        failed: Option<bool>,
    },

    /// A command the agent executed.
    ///
    /// Separate from a tool call because "what did this agent actually run
    /// against my machine" is a question worth answering directly.
    Command {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        command: String,
        exit_code: Option<i32>,
        output: Option<String>,
    },

    /// A file the agent read or changed.
    #[serde(rename = "file_change")]
    FileChange {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        action: FileAction,
        /// The path as the provider recorded it.
        ///
        /// Data, never a path Recall resolves or opens — see
        /// `.github/SECURITY.md`.
        path: PathBuf,
    },

    /// Anything else the provider recorded that belongs in the transcript:
    /// system prompts, notices, mode changes.
    ///
    /// A deliberate catch-all. An adapter that cannot map a record must keep it
    /// rather than drop it, because dropping is the one thing an archive must
    /// never do.
    Other {
        #[serde(
            with = "time::serde::rfc3339::option",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        at: Option<OffsetDateTime>,
        /// What the provider called this record, in its own vocabulary.
        provider_kind: String,
        content: String,
    },
}

impl SessionEvent {
    /// When this happened, if the provider recorded it.
    pub fn at(&self) -> Option<OffsetDateTime> {
        match self {
            SessionEvent::UserMessage { at, .. }
            | SessionEvent::AssistantMessage { at, .. }
            | SessionEvent::ToolCall { at, .. }
            | SessionEvent::ToolResult { at, .. }
            | SessionEvent::Command { at, .. }
            | SessionEvent::FileChange { at, .. }
            | SessionEvent::Other { at, .. } => *at,
        }
    }

    /// A stable name for the variant, for output and for the wire format.
    pub fn kind(&self) -> &'static str {
        match self {
            SessionEvent::UserMessage { .. } => "user",
            SessionEvent::AssistantMessage { .. } => "assistant",
            SessionEvent::ToolCall { .. } => "tool_call",
            SessionEvent::ToolResult { .. } => "tool_result",
            SessionEvent::Command { .. } => "command",
            SessionEvent::FileChange { .. } => "file_change",
            SessionEvent::Other { .. } => "other",
        }
    }

    /// The text a search would want to match against.
    ///
    /// Returns what the event carries, not a summary of it.
    pub fn searchable_text(&self) -> Option<&str> {
        match self {
            SessionEvent::UserMessage { content, .. }
            | SessionEvent::AssistantMessage { content, .. }
            | SessionEvent::ToolResult { content, .. }
            | SessionEvent::Other { content, .. } => Some(content),
            SessionEvent::Command { command, .. } => Some(command),
            SessionEvent::ToolCall { arguments, .. } => arguments.as_deref(),
            SessionEvent::FileChange { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn every_variant_reports_its_kind() {
        let at = Some(datetime!(2026-09-08 12:00:00 UTC));
        let events = [
            SessionEvent::UserMessage {
                at,
                content: "hello".into(),
            },
            SessionEvent::AssistantMessage {
                at,
                content: "hi".into(),
            },
            SessionEvent::ToolCall {
                at,
                name: "read_file".into(),
                arguments: Some("{\"path\":\"src/lib.rs\"}".into()),
                call_id: Some("call-1".into()),
            },
            SessionEvent::ToolResult {
                at,
                call_id: Some("call-1".into()),
                content: "pub fn main() {}".into(),
                failed: Some(false),
            },
            SessionEvent::Command {
                at,
                command: "cargo test".into(),
                exit_code: Some(0),
                output: Some("ok".into()),
            },
            SessionEvent::FileChange {
                at,
                action: FileAction::Modified,
                path: "src/lib.rs".into(),
            },
            SessionEvent::Other {
                at,
                provider_kind: "system_prompt".into(),
                content: "be helpful".into(),
            },
        ];

        let kinds: Vec<_> = events.iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds,
            [
                "user",
                "assistant",
                "tool_call",
                "tool_result",
                "command",
                "file_change",
                "other"
            ]
        );

        // And every one of them can report its timestamp.
        for e in &events {
            assert_eq!(e.at(), at, "{} lost its timestamp", e.kind());
        }
    }

    #[test]
    fn a_timeless_event_is_allowed() {
        // Plenty of formats record order without recording a clock, and an
        // invented timestamp is worse than none.
        let e = SessionEvent::UserMessage {
            at: None,
            content: "hello".into(),
        };
        assert_eq!(e.at(), None);
    }

    #[test]
    fn tool_calls_and_results_keep_everything_they_were_given() {
        let arguments = "{\"path\":\"/etc/hosts\",\"limit\":100}";
        let call = SessionEvent::ToolCall {
            at: None,
            name: "read_file".into(),
            arguments: Some(arguments.into()),
            call_id: Some("call-7".into()),
        };
        let SessionEvent::ToolCall {
            arguments: kept, ..
        } = &call
        else {
            unreachable!()
        };
        assert_eq!(kept.as_deref(), Some(arguments), "arguments were altered");

        // Long, noisy results are the ones most likely to be "helpfully"
        // truncated. They must not be.
        let huge = "x".repeat(200_000);
        let result = SessionEvent::ToolResult {
            at: None,
            call_id: Some("call-7".into()),
            content: huge.clone(),
            failed: Some(true),
        };
        let SessionEvent::ToolResult { content, .. } = &result else {
            unreachable!()
        };
        assert_eq!(content.len(), huge.len());
    }

    #[test]
    fn searchable_text_is_the_content_not_a_summary() {
        assert_eq!(
            SessionEvent::UserMessage {
                at: None,
                content: "payment orchestrator".into()
            }
            .searchable_text(),
            Some("payment orchestrator")
        );
        assert_eq!(
            SessionEvent::Command {
                at: None,
                command: "cargo test".into(),
                exit_code: None,
                output: None
            }
            .searchable_text(),
            Some("cargo test")
        );
        // A file change carries a path, not prose.
        assert_eq!(
            SessionEvent::FileChange {
                at: None,
                action: FileAction::Read,
                path: "src/lib.rs".into()
            }
            .searchable_text(),
            None
        );
    }

    #[test]
    fn a_path_in_an_event_is_data() {
        // Recall records what the agent touched. It never resolves or opens
        // these, however hostile they look — see SECURITY.md.
        let e = SessionEvent::FileChange {
            at: None,
            action: FileAction::Deleted,
            path: "../../../../etc/passwd".into(),
        };
        let SessionEvent::FileChange { path, .. } = &e else {
            unreachable!()
        };
        assert_eq!(path.to_str(), Some("../../../../etc/passwd"));
    }

    #[test]
    fn file_actions_have_stable_names() {
        assert_eq!(FileAction::Read.as_str(), "read");
        assert_eq!(FileAction::Created.as_str(), "created");
        assert_eq!(FileAction::Modified.as_str(), "modified");
        assert_eq!(FileAction::Deleted.as_str(), "deleted");
    }
}
