//! Claude Code's own record types.
//!
//! These mirror what is on disk, not what Recall wants. Mapping them onto
//! Recall's model is #22, and keeping the two apart is what stops Claude's
//! vocabulary leaking into the core.
//!
//! Everything here is defensive. Session files are untrusted input, and the
//! format belongs to someone else: unknown record types, unknown content
//! blocks and unknown fields are all tolerated rather than fatal, because a new
//! Claude Code release adding one must not cost the user their archive.

use serde::Deserialize;

/// One line of a session file.
///
/// Only the fields Recall uses are named. Everything else is ignored, which is
/// deliberate: 21 record types appear in real session files and most carry
/// Claude Code's internal state rather than transcript.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    /// The record type. `user`, `assistant`, `system` and `attachment` carry
    /// conversation; the rest are Claude Code's own bookkeeping.
    #[serde(rename = "type")]
    pub kind: Option<String>,

    /// This record's identity within the session.
    pub uuid: Option<String>,

    /// ISO-8601, UTC.
    pub timestamp: Option<String>,

    /// The working directory the session ran in.
    ///
    /// Exact, unlike the path decoded from the project directory's name.
    pub cwd: Option<String>,

    /// Claude Code's identifier for the session this record belongs to.
    ///
    /// On a sub-agent transcript this is the *parent* session's id.
    pub session_id: Option<String>,

    /// The Claude Code version that wrote the record.
    pub version: Option<String>,

    /// The git branch checked out at the time.
    pub git_branch: Option<String>,

    /// Whether this record came from a sub-agent rather than the main thread.
    #[serde(default)]
    pub is_sidechain: bool,

    /// The message itself, on `user` and `assistant` records.
    pub message: Option<Message>,

    /// What a tool actually did.
    ///
    /// Richer than the tool call: this is where commands and file changes are
    /// derivable from.
    pub tool_use_result: Option<ToolUseResult>,

    /// The payload of an `attachment` record.
    pub attachment: Option<Attachment>,

    /// A `system` record's sub-type.
    pub subtype: Option<String>,

    /// A `system` record's text.
    pub content: Option<serde_json::Value>,
}

impl Record {
    /// Whether this record carries any part of the conversation.
    pub fn is_conversation(&self) -> bool {
        matches!(
            self.kind.as_deref(),
            Some("user" | "assistant" | "system" | "attachment")
        )
    }
}

/// Something shown to the agent on an `attachment` record.
///
/// Most attachments are Claude Code's own injected reminders rather than
/// anything a person or a tool produced — token-count and task reminders alone
/// were 92% of them in the sample this was written against.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub text: Option<String>,
    pub content: Option<serde_json::Value>,
    pub snippet: Option<String>,
    pub filename: Option<String>,
    pub prompt: Option<String>,
}

impl Attachment {
    /// Attachment types Claude Code injects for its own purposes.
    ///
    /// These exist to prompt the model, not to record what happened, and they
    /// outnumber real attachments by more than ten to one. Archiving them would
    /// bury the conversation in bookkeeping.
    ///
    /// The list is a deny-list on purpose: an attachment type not named here is
    /// preserved, so a new one is kept rather than silently lost.
    const INTERNAL: &'static [&'static str] = &[
        "total_tokens_reminder",
        "task_reminder",
        "deferred_tools_delta",
        "agent_listing_delta",
        "mcp_instructions_delta",
        "skill_listing",
        "auto_mode",
    ];

    /// Whether this is Claude Code talking to itself.
    pub fn is_internal(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|k| Self::INTERNAL.contains(&k))
    }

    /// The text worth preserving, if any.
    pub fn text(&self) -> Option<String> {
        if let Some(t) = self.snippet.as_deref().or(self.text.as_deref()) {
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        if let Some(p) = self.prompt.as_deref().filter(|p| !p.is_empty()) {
            return Some(p.to_string());
        }
        match self.content.as_ref() {
            Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(serde_json::Value::Null) | None => None,
            Some(other) => Some(other.to_string()),
        }
    }

    /// A label for this attachment, for the event's provider kind.
    pub fn label(&self) -> String {
        match (&self.kind, &self.filename) {
            (Some(k), Some(f)) => format!("attachment:{k}:{f}"),
            (Some(k), None) => format!("attachment:{k}"),
            (None, _) => "attachment".to_string(),
        }
    }
}

/// The message on a `user` or `assistant` record.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: Option<String>,
    /// Present on assistant messages.
    pub model: Option<String>,
    pub content: Option<Content>,
}

/// A message body.
///
/// String on some records and an array of blocks on others — both occur in real
/// sessions, and a parser that handles only one silently drops the other.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

/// One piece of a message.
///
/// Untagged with a catch-all so an unrecognised block type is kept as raw JSON
/// rather than failing the session. Dropping is the one thing an archive must
/// not do.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Block {
    Known(KnownBlock),
    Unknown(serde_json::Value),
}

/// The block types Recall understands.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: Option<String>,
        name: String,
        input: Option<serde_json::Value>,
    },
    ToolResult {
        tool_use_id: Option<String>,
        content: Option<Content>,
        #[serde(default)]
        is_error: Option<bool>,
    },
    /// Extended thinking. Recall has no variant for it, so #22 preserves it as
    /// `Other { provider_kind: "thinking" }`.
    Thinking {
        thinking: String,
    },
}

/// What a tool did, as Claude Code recorded it.
///
/// **Not always an object.** When a tool fails or the user rejects it, Claude
/// Code writes a bare string here instead — "User rejected tool use", or the
/// tool's error text. In the installation this adapter was written against that
/// accounted for 1,061 records, every one of them a `user` record carrying real
/// conversation. A parser that insisted on an object dropped all of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResult {
    /// The usual shape: what the tool produced.
    Structured(Box<StructuredResult>),
    /// An error or rejection, recorded as plain text.
    Text(String),
    /// Something else entirely. Kept rather than dropped.
    Other(serde_json::Value),
}

impl ToolUseResult {
    /// The structured form, when there is one.
    pub fn structured(&self) -> Option<&StructuredResult> {
        match self {
            ToolUseResult::Structured(r) => Some(r),
            _ => None,
        }
    }

    /// Whether this looks like a command having been run.
    pub fn is_command(&self) -> bool {
        self.structured().is_some_and(StructuredResult::is_command)
    }

    /// The file this touched, if it touched one.
    pub fn touched_file(&self) -> Option<&str> {
        self.structured().and_then(StructuredResult::touched_file)
    }

    /// Whether the file was changed rather than only read.
    pub fn changed_the_file(&self) -> bool {
        self.structured()
            .is_some_and(StructuredResult::changed_the_file)
    }

    /// Text Recall should preserve, when the result is not structured.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolUseResult::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// The object form of a tool result.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredResult {
    /// A command ran.
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    #[serde(default)]
    pub interrupted: Option<bool>,

    /// A file was touched.
    pub file_path: Option<String>,
    /// Present when the file was edited rather than only read.
    pub structured_patch: Option<serde_json::Value>,
    pub original_file: Option<String>,
    /// Present on a string replacement.
    pub old_string: Option<String>,
    pub new_string: Option<String>,
}

impl StructuredResult {
    /// Whether this looks like a command having been run.
    pub fn is_command(&self) -> bool {
        self.stdout.is_some() || self.stderr.is_some()
    }

    /// Whether this looks like a file having been touched.
    pub fn touched_file(&self) -> Option<&str> {
        self.file_path.as_deref()
    }

    /// Whether the file was changed rather than only read.
    pub fn changed_the_file(&self) -> bool {
        self.structured_patch.is_some() || self.new_string.is_some()
    }
}
