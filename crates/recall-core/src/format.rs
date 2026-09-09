//! How a session is written down.
//!
//! JSON Lines: a header line carrying the session's metadata, then one line per
//! event in order.
//!
//! Line-delimited rather than one JSON document on purpose. A transcript can be
//! hundreds of megabytes, and this shape lets both writing and reading work an
//! event at a time — memory scales with the largest single event, not with the
//! session. #16 and #17 compress and decompress this stream without having to
//! reshape it.
//!
//! It is also a text format, which means a damaged archive can still be
//! inspected by hand. That mattered more than the bytes a binary encoding would
//! have saved, since sessions are compressed anyway.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::event::SessionEvent;
use crate::session::{Session, SessionId};

/// The session encoding this build reads and writes.
///
/// Distinct from the archive's `config.toml` `format_version`, which describes
/// the directory layout. This one describes what is inside a session file; the
/// two can move independently.
pub const SESSION_FORMAT_VERSION: u32 = 1;

/// The first line of a session file.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    format: u32,
    session: Session,
}

/// Why a session could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    /// The underlying reader or writer failed.
    #[error("i/o error while {doing}")]
    Io {
        doing: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// The file was empty, so there is no header to read.
    #[error("session file is empty")]
    Empty,

    /// The bytes are not text.
    ///
    /// Distinguished from an i/o failure because it means something entirely
    /// different: the file was read fine, it just is not a session. Reporting
    /// it as "i/o error" sends whoever is diagnosing it looking at the disk.
    #[error("line {line} is not valid UTF-8, so this is not a session file")]
    NotUtf8 { line: usize },

    /// A line was not valid JSON, or did not match the shape expected.
    ///
    /// The line number is 1-based and counts the header, so it points at the
    /// line a person would find in the file.
    #[error("line {line} is not a valid session record")]
    Malformed {
        line: usize,
        #[source]
        source: serde_json::Error,
    },

    /// Written by a version of Recall this build does not understand.
    ///
    /// Fatal by design: guessing at an unknown encoding is how an archive gets
    /// misread. See `.github/SECURITY.md`.
    #[error("session format version {found} is not supported (this build reads {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },

    /// The stored id does not match the provider fields stored beside it.
    ///
    /// The id is derived from those fields, so a mismatch means the file was
    /// edited, corrupted, or written by something that did not derive it the
    /// same way. Either way the file is not trustworthy.
    #[error(
        "session id {stored} does not match the provider fields it should derive from ({expected})"
    )]
    IdMismatch {
        stored: SessionId,
        expected: SessionId,
    },
}

/// Tell "these bytes are not text" apart from "the disk failed".
fn classify(line: usize, doing: &'static str, source: std::io::Error) -> FormatError {
    if source.kind() == std::io::ErrorKind::InvalidData {
        FormatError::NotUtf8 { line }
    } else {
        FormatError::Io { doing, source }
    }
}

/// Write a session as JSON Lines.
///
/// Streams: one event is serialized at a time, so the writer never holds more
/// than a single event beyond what the caller already had.
pub fn write_session<W: Write>(mut out: W, session: &Session) -> Result<(), FormatError> {
    let header = Header {
        format: SESSION_FORMAT_VERSION,
        // events are `#[serde(skip)]`, so this writes metadata only.
        session: session.clone(),
    };
    let line = serde_json::to_string(&header)
        .map_err(|source| FormatError::Malformed { line: 1, source })?;
    writeln!(out, "{line}").map_err(|source| FormatError::Io {
        doing: "writing the session header",
        source,
    })?;

    for (i, event) in session.events.iter().enumerate() {
        let line = serde_json::to_string(event).map_err(|source| FormatError::Malformed {
            line: i + 2,
            source,
        })?;
        writeln!(out, "{line}").map_err(|source| FormatError::Io {
            doing: "writing a session event",
            source,
        })?;
    }
    Ok(())
}

/// Read a session written by [`write_session`].
///
/// Streams the same way: lines are consumed one at a time.
pub fn read_session<R: BufRead>(input: R) -> Result<Session, FormatError> {
    let mut lines = input.lines().enumerate();

    let (_, first) = lines.next().ok_or(FormatError::Empty)?;
    let first = first.map_err(|source| classify(1, "reading the session header", source))?;
    if first.trim().is_empty() {
        return Err(FormatError::Empty);
    }

    let header: Header = serde_json::from_str(&first)
        .map_err(|source| FormatError::Malformed { line: 1, source })?;
    if header.format != SESSION_FORMAT_VERSION {
        return Err(FormatError::UnsupportedVersion {
            found: header.format,
            supported: SESSION_FORMAT_VERSION,
        });
    }

    let mut session = header.session;

    // The id is derived from the provider fields, so it has to agree with them.
    // Checking here turns a silently wrong archive into a loud one.
    let expected = SessionId::derive(&session.provider, &session.provider_session_id);
    if session.id != expected {
        return Err(FormatError::IdMismatch {
            stored: session.id,
            expected,
        });
    }

    for (i, line) in lines {
        let line = line.map_err(|source| classify(i + 1, "reading a session event", source))?;
        // A trailing newline at end of file is normal, not an error.
        if line.trim().is_empty() {
            continue;
        }
        let event: SessionEvent =
            serde_json::from_str(&line).map_err(|source| FormatError::Malformed {
                line: i + 1,
                source,
            })?;
        session.events.push(event);
    }

    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::FileAction;
    use crate::session::{GitContext, Provider};
    use time::macros::datetime;

    fn provider() -> Provider {
        Provider::new("claude-code").expect("valid provider")
    }

    fn session() -> Session {
        Session::new(provider(), "abc-123", datetime!(2026-09-08 12:00:00 UTC))
    }

    /// Write then read. Anything that does not come back identical is data loss.
    fn round_trip(session: &Session) -> Session {
        let mut buf = Vec::new();
        write_session(&mut buf, session).expect("write");
        read_session(buf.as_slice()).expect("read")
    }

    #[test]
    fn a_bare_session_survives() {
        let s = session();
        assert_eq!(round_trip(&s), s);
    }

    #[test]
    fn every_metadata_field_survives() {
        let mut s = session();
        s.model = Some("claude-opus-5".into());
        s.ended_at = Some(datetime!(2026-09-08 14:30:00 UTC));
        s.project = Some("/home/me/project".into());
        s.git = Some(GitContext {
            repository: Some("/home/me/project".into()),
            branch: Some("feature/10-session-type".into()),
            commit_at_start: Some("a".repeat(40)),
            commit_at_end: Some("b".repeat(40)),
        });
        assert_eq!(round_trip(&s), s);
    }

    #[test]
    fn every_event_variant_survives() {
        let at = Some(datetime!(2026-09-08 12:00:00 UTC));
        let mut s = session();
        s.events = vec![
            SessionEvent::UserMessage {
                at,
                content: "design the archive".into(),
            },
            SessionEvent::AssistantMessage {
                at: None,
                content: "here is a plan".into(),
            },
            SessionEvent::ToolCall {
                at,
                name: "read_file".into(),
                arguments: Some(r#"{"path":"src/lib.rs","limit":100}"#.into()),
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
                command: "cargo test --workspace".into(),
                exit_code: Some(0),
                output: Some("29 passed".into()),
            },
            SessionEvent::FileChange {
                at,
                action: FileAction::Modified,
                path: "crates/recall-core/src/lib.rs".into(),
            },
            SessionEvent::Other {
                at,
                provider_kind: "system_prompt".into(),
                content: "be concise".into(),
            },
        ];
        assert_eq!(round_trip(&s), s);
    }

    #[test]
    fn event_order_is_preserved() {
        // Order is the transcript.
        let mut s = session();
        s.events = (0..500)
            .map(|i| SessionEvent::UserMessage {
                at: None,
                content: format!("message {i}"),
            })
            .collect();
        let back = round_trip(&s);
        assert_eq!(back.events, s.events);
    }

    #[test]
    fn awkward_content_survives() {
        // The characters most likely to break a line-delimited format.
        let mut s = session();
        for content in [
            String::new(),
            "embedded\nnewline\nand\rcarriage return".into(),
            "unicode: 日本語 — émoji 🧠 ✅".into(),
            "quotes \" backslash \\ and a literal \\n".into(),
            "tab\tseparated\tvalues".into(),
            "json-looking: {\"kind\":\"user\",\"content\":\"not really\"}".into(),
            "\u{0}\u{1}\u{7f} control characters".into(),
        ] {
            s.events
                .push(SessionEvent::UserMessage { at: None, content });
        }
        assert_eq!(round_trip(&s), s);
    }

    #[test]
    fn a_very_large_tool_result_survives() {
        // Long, noisy results are exactly what a naive implementation would
        // truncate. 2 MB in one event.
        let mut s = session();
        s.events.push(SessionEvent::ToolResult {
            at: None,
            call_id: None,
            content: "x".repeat(2 * 1024 * 1024),
            failed: None,
        });
        let back = round_trip(&s);
        assert_eq!(back, s);
    }

    #[test]
    fn the_header_is_one_line_and_each_event_is_one_line() {
        // The property #16 and #17 rely on to stream.
        let mut s = session();
        s.events = vec![
            SessionEvent::UserMessage {
                at: None,
                content: "line\nwith\nnewlines".into(),
            },
            SessionEvent::AssistantMessage {
                at: None,
                content: "another".into(),
            },
        ];
        let mut buf = Vec::new();
        write_session(&mut buf, &s).expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert_eq!(
            text.lines().count(),
            3,
            "expected header + 2 events, got:\n{text}"
        );
    }

    #[test]
    fn an_empty_file_is_an_error_not_an_empty_session() {
        let err = read_session(&b""[..]).expect_err("empty input must fail");
        assert!(matches!(err, FormatError::Empty), "{err:?}");
    }

    #[test]
    fn an_unknown_format_version_is_refused() {
        let line = r#"{"format":99,"session":{"id":"00","provider":"claude-code","provider_session_id":"a","model":null,"started_at":"2026-09-08T12:00:00Z","ended_at":null,"project":null,"git":null}}"#;
        let err = read_session(line.as_bytes()).expect_err("unknown version must fail");
        assert!(
            matches!(err, FormatError::UnsupportedVersion { found: 99, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_truncated_event_line_is_reported_with_its_line_number() {
        let mut buf = Vec::new();
        let mut s = session();
        s.events.push(SessionEvent::UserMessage {
            at: None,
            content: "fine".into(),
        });
        write_session(&mut buf, &s).expect("write");
        let mut text = String::from_utf8(buf).expect("utf8");
        text.push_str("{\"kind\":\"user\",\"conte\n"); // cut mid-record

        let err = read_session(text.as_bytes()).expect_err("truncation must fail");
        match err {
            FormatError::Malformed { line, .. } => assert_eq!(line, 3),
            other => panic!("expected a malformed-line error, got {other:?}"),
        }
    }

    #[test]
    fn a_tampered_id_is_refused() {
        // The id derives from the provider fields. If they disagree, the file
        // was edited or corrupted and is not trustworthy.
        let mut buf = Vec::new();
        write_session(&mut buf, &session()).expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        let tampered = text.replace(
            "\"provider_session_id\":\"abc-123\"",
            "\"provider_session_id\":\"different\"",
        );
        assert_ne!(tampered, text, "the fixture did not actually change");

        let err = read_session(tampered.as_bytes()).expect_err("id mismatch must fail");
        assert!(matches!(err, FormatError::IdMismatch { .. }), "{err:?}");
    }

    #[test]
    fn an_unknown_event_kind_is_refused_rather_than_guessed() {
        let mut buf = Vec::new();
        write_session(&mut buf, &session()).expect("write");
        let mut text = String::from_utf8(buf).expect("utf8");
        text.push_str("{\"kind\":\"telepathy\",\"content\":\"?\"}\n");

        let err = read_session(text.as_bytes()).expect_err("unknown kind must fail");
        assert!(
            matches!(err, FormatError::Malformed { line: 2, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn bytes_that_are_not_text_say_so() {
        // Reporting this as an i/o error would send whoever is diagnosing it
        // looking at the disk, when the file is simply not a session.
        let err = read_session(&[0xff, 0x00, 0xfe, 0x42][..]).expect_err("must fail");
        assert!(matches!(err, FormatError::NotUtf8 { line: 1 }), "{err:?}");
        assert!(
            !format!("{err}").contains("i/o"),
            "still described as an i/o error: {err}"
        );
    }

    #[test]
    fn invalid_utf8_in_an_event_names_its_line() {
        let mut buf = Vec::new();
        write_session(&mut buf, &session()).expect("write");
        buf.extend_from_slice(&[0xff, 0xfe, b'\n']);

        let err = read_session(buf.as_slice()).expect_err("must fail");
        assert!(matches!(err, FormatError::NotUtf8 { line: 2 }), "{err:?}");
    }

    #[test]
    fn a_trailing_blank_line_is_not_an_error() {
        let mut buf = Vec::new();
        let s = session();
        write_session(&mut buf, &s).expect("write");
        let mut text = String::from_utf8(buf).expect("utf8");
        text.push('\n');
        assert_eq!(read_session(text.as_bytes()).expect("read"), s);
    }
}
