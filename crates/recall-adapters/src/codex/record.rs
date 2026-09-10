//! Codex's own record types.
//!
//! These mirror what is on disk, not what Recall wants. Mapping them onto
//! Recall's model is [`super::normalize`], and keeping the two apart is what
//! stops Codex's vocabulary leaking into the core.
//!
//! Everything here is defensive. Rollouts are untrusted input, and the format
//! belongs to someone else: a single history mixes several Codex releases, so
//! unknown record types, unknown payload types and unknown fields are all
//! tolerated rather than fatal.

use serde::Deserialize;

/// One line of a rollout.
///
/// Every line has the same envelope; what it carries is inside `payload`, whose
/// shape depends on `kind`.
#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    /// The outer record type: `session_meta`, `response_item`, `turn_context`,
    /// `event_msg` or `world_state`.
    #[serde(rename = "type")]
    pub kind: Option<String>,

    /// ISO-8601, UTC. Present on every record in the sample this was written
    /// against, but optional here because absence must not be fatal.
    pub timestamp: Option<String>,

    /// The record's position in the file, as Codex numbered it.
    pub ordinal: Option<u64>,

    /// The record body. Read into a typed view only once `kind` says which one
    /// applies, so an unexpected shape costs a record rather than the file.
    pub payload: Option<serde_json::Value>,
}

/// Outer record types this adapter understands.
impl Record {
    /// The session header, written first in every rollout.
    pub const SESSION_META: &'static str = "session_meta";
    /// A transcript item: a message, a tool call, a tool result.
    pub const RESPONSE_ITEM: &'static str = "response_item";
    /// The model and settings a turn ran under.
    pub const TURN_CONTEXT: &'static str = "turn_context";

    /// Read the payload as a session header, if this is one.
    pub fn session_meta(&self) -> Option<SessionMeta> {
        self.payload_as(Self::SESSION_META)
    }

    /// Read the payload as a turn context, if this is one.
    pub fn turn_context(&self) -> Option<TurnContext> {
        self.payload_as(Self::TURN_CONTEXT)
    }

    /// Read the payload as a transcript item, if this is one.
    ///
    /// An item whose `type` is not one this adapter names comes back as
    /// [`Item::Unknown`] rather than as nothing, so a new Codex release cannot
    /// quietly cost the user part of a conversation.
    pub fn response_item(&self) -> Option<Item> {
        self.payload_as(Self::RESPONSE_ITEM)
    }

    fn payload_as<T: for<'de> Deserialize<'de>>(&self, kind: &str) -> Option<T> {
        if self.kind.as_deref() != Some(kind) {
            return None;
        }
        serde_json::from_value(self.payload.clone()?).ok()
    }
}

/// The `session_meta` payload.
///
/// The only place a rollout records its own identity, its working directory,
/// and the Codex version that wrote it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionMeta {
    /// Codex's identifier for the session.
    pub session_id: Option<String>,
    /// The same identifier under its other name. Both appear on every header.
    pub id: Option<String>,
    /// The working directory the session ran in.
    pub cwd: Option<String>,
    /// The Codex version that wrote the rollout.
    pub cli_version: Option<String>,
    /// The system prompt the session ran under.
    pub base_instructions: Option<BaseInstructions>,
}

impl SessionMeta {
    /// The session's identifier, under whichever name it was recorded.
    ///
    /// Each name is checked for emptiness before falling through to the next:
    /// a header carrying `"session_id": ""` has to reach `id` rather than
    /// stopping on the blank it found first.
    pub fn identifier(&self) -> Option<&str> {
        let usable = |s: &&String| !s.is_empty();
        self.session_id
            .as_ref()
            .filter(usable)
            .or_else(|| self.id.as_ref().filter(usable))
            .map(String::as_str)
    }
}

/// The system prompt, as `session_meta` records it.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BaseInstructions {
    /// The usual shape: an object with the prompt under `text`.
    Structured { text: Option<String> },
    /// Recorded as a bare string by some releases.
    Text(String),
}

impl BaseInstructions {
    /// The prompt text, if there is any.
    pub fn text(&self) -> Option<&str> {
        match self {
            BaseInstructions::Structured { text } => text.as_deref(),
            BaseInstructions::Text(text) => Some(text.as_str()),
        }
        .filter(|t| !t.is_empty())
    }
}

/// The `turn_context` payload.
///
/// Written once per turn. The model is here rather than on the messages
/// themselves, which is why normalization reads it separately.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TurnContext {
    /// The model the turn ran against.
    pub model: Option<String>,
    /// The working directory the turn ran in.
    pub cwd: Option<String>,
}

/// One transcript item.
///
/// Split the way Claude Code's content blocks are: a named shape when this
/// adapter recognises the item, and the raw value when it does not. An item it
/// cannot name is preserved rather than dropped.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Item {
    Known(KnownItem),
    Unknown(serde_json::Value),
}

/// The transcript items this adapter maps onto Recall's model.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownItem {
    /// Something said, by the person or by the model.
    Message {
        /// `user`, `assistant`, or `developer`.
        role: Option<String>,
        content: Option<Vec<ContentItem>>,
    },

    /// The model called a tool.
    FunctionCall {
        name: Option<String>,
        /// A JSON object encoded as a string. Kept verbatim.
        arguments: Option<String>,
        call_id: Option<String>,
    },

    /// What a tool returned.
    FunctionCallOutput {
        call_id: Option<String>,
        /// Usually a plain string; occasionally a list of content items.
        output: Option<serde_json::Value>,
    },

    /// A tool called through Codex's custom-tool path rather than as a
    /// function.
    CustomToolCall {
        name: Option<String>,
        input: Option<serde_json::Value>,
        call_id: Option<String>,
    },

    /// What a custom tool returned.
    CustomToolCallOutput {
        call_id: Option<String>,
        output: Option<serde_json::Value>,
    },

    /// The model's reasoning, as summary text.
    ///
    /// `encrypted_content` is deliberately not named: it is opaque, it is the
    /// largest field on the record, and it cannot be read back by anyone.
    Reasoning {
        summary: Option<Vec<ContentItem>>,
        content: Option<Vec<ContentItem>>,
    },
}

/// One piece of an item's body.
///
/// `input_text`, `output_text` and `summary_text` all have the same two fields,
/// so one type covers them.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub text: Option<String>,
}

impl ContentItem {
    /// The text this piece carries, if it carries any.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref().filter(|t| !t.is_empty())
    }
}

/// The name of the tool Codex runs shell commands through.
pub const EXEC_COMMAND_TOOL: &str = "exec_command";

/// The arguments to [`EXEC_COMMAND_TOOL`].
///
/// Read so the command itself can be recorded as a [`recall_core::SessionEvent::Command`].
/// Claude Code cannot supply this — it records what a command produced but not
/// what was run — so Codex answers "what did this agent run against my machine"
/// more directly than the first adapter did.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExecCommandArguments {
    /// The command line, as a single string.
    pub cmd: Option<String>,
    /// The directory it ran in.
    pub workdir: Option<String>,
}

impl ExecCommandArguments {
    /// Read the arguments of an `exec_command` call.
    ///
    /// The arguments are a JSON object encoded as a string. A call whose
    /// arguments cannot be read still becomes a tool call — only the derived
    /// command is lost.
    pub fn parse(arguments: &str) -> Option<Self> {
        let parsed: Self = serde_json::from_str(arguments).ok()?;
        parsed.cmd.is_some().then_some(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(line: &str) -> Record {
        serde_json::from_str(line).expect("record")
    }

    #[test]
    fn a_session_header_is_read() {
        let r = record(
            r#"{"type":"session_meta","timestamp":"2026-08-03T13:11:04.855Z","ordinal":0,
                "payload":{"session_id":"s1","id":"s1","cwd":"/w","cli_version":"0.152.1",
                "base_instructions":{"text":"be pragmatic"}}}"#,
        );
        let meta = r.session_meta().expect("session meta");
        assert_eq!(meta.identifier(), Some("s1"));
        assert_eq!(meta.cwd.as_deref(), Some("/w"));
        assert_eq!(meta.cli_version.as_deref(), Some("0.152.1"));
        assert_eq!(
            meta.base_instructions.as_ref().and_then(|b| b.text()),
            Some("be pragmatic")
        );
    }

    #[test]
    fn the_identifier_falls_back_to_the_other_name_it_is_recorded_under() {
        let meta = SessionMeta {
            id: Some("only-id".into()),
            ..SessionMeta::default()
        };
        assert_eq!(meta.identifier(), Some("only-id"));

        // Present but empty is the same as absent.
        let blank = SessionMeta {
            session_id: Some(String::new()),
            id: Some("fallback".into()),
            ..SessionMeta::default()
        };
        assert_eq!(blank.identifier(), Some("fallback"));
    }

    #[test]
    fn a_payload_is_only_read_as_the_type_the_record_says_it_is() {
        // Reading a turn context as a session header would invent a session id
        // out of an unrelated record.
        let r = record(r#"{"type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/w"}}"#);
        assert!(r.session_meta().is_none());
        assert_eq!(
            r.turn_context().expect("turn context").model.as_deref(),
            Some("gpt-5.5")
        );
    }

    #[test]
    fn a_message_is_read_with_its_role_and_body() {
        let r = record(
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"done"}]}}"#,
        );
        let Some(Item::Known(KnownItem::Message { role, content })) = r.response_item() else {
            panic!("expected a message");
        };
        assert_eq!(role.as_deref(), Some("assistant"));
        assert_eq!(content.expect("content")[0].text(), Some("done"));
    }

    #[test]
    fn a_tool_call_and_its_output_are_read() {
        let call = record(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command",
                "arguments":"{\"cmd\":\"cargo test\"}","call_id":"c1"}}"#,
        );
        let Some(Item::Known(KnownItem::FunctionCall {
            name,
            arguments,
            call_id,
        })) = call.response_item()
        else {
            panic!("expected a function call");
        };
        assert_eq!(name.as_deref(), Some("exec_command"));
        assert_eq!(call_id.as_deref(), Some("c1"));
        let args = ExecCommandArguments::parse(&arguments.expect("arguments")).expect("parsed");
        assert_eq!(args.cmd.as_deref(), Some("cargo test"));

        let output = record(
            r#"{"type":"response_item","payload":{"type":"function_call_output",
                "call_id":"c1","output":"ok"}}"#,
        );
        let Some(Item::Known(KnownItem::FunctionCallOutput { call_id, output })) =
            output.response_item()
        else {
            panic!("expected a function call output");
        };
        assert_eq!(call_id.as_deref(), Some("c1"));
        assert_eq!(output.expect("output").as_str(), Some("ok"));
    }

    #[test]
    fn exec_arguments_that_cannot_be_read_yield_nothing_rather_than_a_guess() {
        assert!(ExecCommandArguments::parse("not json").is_none());
        // Well-formed, but no command in it. Inventing an empty one would
        // claim the agent ran something it did not.
        assert!(ExecCommandArguments::parse(r#"{"workdir":"/w"}"#).is_none());
    }

    #[test]
    fn an_unrecognised_item_is_kept_rather_than_dropped() {
        // A history spans several Codex releases, so this is the ordinary case
        // for anything added after this adapter was written.
        let r = record(
            r#"{"type":"response_item","payload":{"type":"something_new","detail":"kept"}}"#,
        );
        let Some(Item::Unknown(value)) = r.response_item() else {
            panic!("an unknown item was not preserved");
        };
        assert_eq!(value.get("detail").and_then(|d| d.as_str()), Some("kept"));
    }

    #[test]
    fn unknown_fields_on_a_known_item_do_not_cost_the_record() {
        let r = record(
            r#"{"type":"response_item","payload":{"type":"message","role":"user",
                "content":[{"type":"input_text","text":"hi"}],"added_in_a_later_release":true}}"#,
        );
        assert!(matches!(
            r.response_item(),
            Some(Item::Known(KnownItem::Message { .. }))
        ));
    }

    #[test]
    fn a_system_prompt_recorded_as_a_bare_string_is_still_read() {
        let r = record(
            r#"{"type":"session_meta","payload":{"session_id":"s1","base_instructions":"plain"}}"#,
        );
        assert_eq!(
            r.session_meta()
                .expect("meta")
                .base_instructions
                .as_ref()
                .and_then(|b| b.text()),
            Some("plain")
        );
    }
}
