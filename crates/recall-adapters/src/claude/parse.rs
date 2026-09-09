//! Reading a Claude Code session file into [`Record`]s.
//!
//! Line-oriented and forgiving. A session file is untrusted input written by
//! software this project does not control, so a line that cannot be understood
//! costs that line rather than the session.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use recall_core::{AdapterError, Provider};

use super::record::Record;

/// What one session file contained.
#[derive(Debug, Default)]
pub struct ParsedFile {
    /// Records that parsed, in file order.
    pub records: Vec<Record>,
    /// Lines that did not parse.
    ///
    /// Counted rather than discarded silently: a session that lost lines is
    /// worth knowing about even when the rest of it is fine.
    pub unreadable_lines: usize,
}

impl ParsedFile {
    /// Whether anything at all could be read.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Read one session file.
///
/// The file is opened read-only and never modified — see
/// `.github/SECURITY.md`.
pub fn parse_file(
    provider: &Provider,
    provider_session_id: &str,
    path: &Path,
) -> Result<ParsedFile, AdapterError> {
    let file = File::open(path).map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut parsed = ParsedFile::default();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            // A line that is not text is not a record. One of them must not
            // cost the rest of the session.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                parsed.unreadable_lines += 1;
                continue;
            }
            Err(source) => {
                return Err(AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match serde_json::from_str::<Record>(line) {
            Ok(record) => parsed.records.push(record),
            Err(_) => parsed.unreadable_lines += 1,
        }
    }

    // A file with nothing readable in it is worth reporting, since archiving an
    // empty session would claim the conversation was empty rather than lost.
    if parsed.is_empty() && parsed.unreadable_lines > 0 {
        return Err(AdapterError::Malformed {
            provider: provider.clone(),
            provider_session_id: provider_session_id.to_string(),
            detail: format!(
                "none of the {} lines in {} could be read",
                parsed.unreadable_lines,
                path.display()
            ),
        });
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::record::{Block, Content, KnownBlock};

    fn provider() -> Provider {
        Provider::new("claude-code").expect("provider")
    }

    fn parse(lines: &str) -> ParsedFile {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, lines).expect("write");
        parse_file(&provider(), "test", &path).expect("parse")
    }

    #[test]
    fn a_user_message_is_read() {
        let p = parse(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-09-08T12:00:00.000Z","cwd":"/w","sessionId":"s","version":"2.1.228","gitBranch":"dev","message":{"role":"user","content":"hello"}}"#,
        );
        assert_eq!(p.records.len(), 1);
        let r = &p.records[0];
        assert_eq!(r.kind.as_deref(), Some("user"));
        assert_eq!(r.cwd.as_deref(), Some("/w"));
        assert_eq!(r.git_branch.as_deref(), Some("dev"));
        assert_eq!(r.version.as_deref(), Some("2.1.228"));
        assert!(!r.is_sidechain);
        assert!(matches!(
            r.message.as_ref().and_then(|m| m.content.clone()),
            Some(Content::Text(t)) if t == "hello"
        ));
    }

    #[test]
    fn content_is_read_whether_it_is_a_string_or_an_array() {
        // Both occur in real sessions. Handling only one silently drops the
        // other, and the array form is the overwhelming majority.
        let as_string = parse(r#"{"type":"user","message":{"content":"plain"}}"#);
        assert!(matches!(
            as_string.records[0].message.as_ref().unwrap().content,
            Some(Content::Text(_))
        ));

        let as_array =
            parse(r#"{"type":"user","message":{"content":[{"type":"text","text":"blocked"}]}}"#);
        assert!(matches!(
            as_array.records[0].message.as_ref().unwrap().content,
            Some(Content::Blocks(_))
        ));
    }

    #[test]
    fn the_four_known_block_types_are_recognised() {
        let p = parse(
            r#"{"type":"assistant","message":{"model":"claude-opus-5","content":[
                {"type":"text","text":"thinking about it"},
                {"type":"thinking","thinking":"internal","signature":"sig"},
                {"type":"tool_use","id":"t1","name":"read_file","input":{"path":"a.rs"}},
                {"type":"tool_result","tool_use_id":"t1","content":"contents","is_error":false}
            ]}}"#
                .replace('\n', "")
                .as_str(),
        );
        let Some(Content::Blocks(blocks)) = &p.records[0].message.as_ref().unwrap().content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], Block::Known(KnownBlock::Text { .. })));
        assert!(matches!(
            blocks[1],
            Block::Known(KnownBlock::Thinking { .. })
        ));
        assert!(matches!(
            blocks[2],
            Block::Known(KnownBlock::ToolUse { .. })
        ));
        assert!(matches!(
            blocks[3],
            Block::Known(KnownBlock::ToolResult { .. })
        ));
        assert_eq!(
            p.records[0].message.as_ref().unwrap().model.as_deref(),
            Some("claude-opus-5")
        );
    }

    #[test]
    fn an_unknown_block_type_is_kept_rather_than_dropped() {
        // A future Claude Code release adding a block type must not cost the
        // user the rest of the session.
        let p = parse(
            r#"{"type":"assistant","message":{"content":[{"type":"holographic","payload":{"x":1}}]}}"#,
        );
        let Some(Content::Blocks(blocks)) = &p.records[0].message.as_ref().unwrap().content else {
            panic!("expected blocks");
        };
        assert!(matches!(blocks[0], Block::Unknown(_)));
    }

    #[test]
    fn unknown_record_types_and_unknown_fields_are_tolerated() {
        let p = parse(
            "{\"type\":\"ai-title\",\"aiTitle\":\"x\",\"sessionId\":\"s\"}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"hi\"},\"somethingNew\":42}\n",
        );
        assert_eq!(p.records.len(), 2);
        assert_eq!(p.unreadable_lines, 0);
        assert!(!p.records[0].is_conversation());
        assert!(p.records[1].is_conversation());
    }

    #[test]
    fn a_tool_use_result_is_read() {
        let p = parse(
            r#"{"type":"user","toolUseResult":{"stdout":"ok","stderr":"","interrupted":false}}"#,
        );
        let r = p.records[0].tool_use_result.as_ref().expect("result");
        assert!(r.is_command());
        assert_eq!(r.touched_file(), None);

        let p = parse(
            r#"{"type":"user","toolUseResult":{"filePath":"/w/a.rs","structuredPatch":[],"originalFile":"x"}}"#,
        );
        let r = p.records[0].tool_use_result.as_ref().expect("result");
        assert_eq!(r.touched_file(), Some("/w/a.rs"));
        assert!(r.changed_the_file());
    }

    #[test]
    fn a_tool_result_recorded_as_plain_text_is_kept() {
        // Claude Code writes a bare string here when a tool fails or the user
        // rejects it. Insisting on an object dropped 1,061 real user records -
        // conversation, not bookkeeping - in the installation this was checked
        // against.
        for text in [
            r#"{"type":"user","toolUseResult":"User rejected tool use"}"#,
            r#"{"type":"user","toolUseResult":"Error: File does not exist."}"#,
        ] {
            let p = parse(text);
            assert_eq!(p.records.len(), 1, "record was dropped: {text}");
            assert_eq!(p.unreadable_lines, 0);
            let r = p.records[0].tool_use_result.as_ref().expect("result");
            assert!(r.as_text().is_some(), "text form was not preserved");
            assert!(!r.is_command());
            assert_eq!(r.touched_file(), None);
        }
    }

    #[test]
    fn a_tool_result_of_an_unexpected_shape_is_still_kept() {
        let p = parse(r#"{"type":"user","toolUseResult":[1,2,3]}"#);
        assert_eq!(p.records.len(), 1);
        assert!(p.records[0].tool_use_result.is_some());
    }

    #[test]
    fn a_file_that_was_only_read_is_not_reported_as_changed() {
        let p = parse(r#"{"type":"user","toolUseResult":{"filePath":"/w/a.rs"}}"#);
        let r = p.records[0].tool_use_result.as_ref().expect("result");
        assert_eq!(r.touched_file(), Some("/w/a.rs"));
        assert!(!r.changed_the_file());
    }

    #[test]
    fn a_bad_line_costs_only_that_line() {
        let p = parse(
            "{\"type\":\"user\",\"message\":{\"content\":\"one\"}}\n\
             {this is not json\n\
             {\"type\":\"user\",\"message\":{\"content\":\"two\"}}\n",
        );
        assert_eq!(p.records.len(), 2, "a bad line took good records with it");
        assert_eq!(p.unreadable_lines, 1);
    }

    #[test]
    fn blank_lines_are_not_counted_as_damage() {
        let p = parse("{\"type\":\"user\"}\n\n   \n{\"type\":\"user\"}\n");
        assert_eq!(p.records.len(), 2);
        assert_eq!(p.unreadable_lines, 0);
    }

    #[test]
    fn an_empty_file_parses_to_nothing_without_erroring() {
        let p = parse("");
        assert!(p.is_empty());
        assert_eq!(p.unreadable_lines, 0);
    }

    #[test]
    fn a_file_where_nothing_is_readable_is_an_error() {
        // Archiving this as an empty session would claim the conversation was
        // empty rather than unreadable.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "not json\nalso not json\n").expect("write");

        let err = parse_file(&provider(), "test", &path).expect_err("must fail");
        assert!(matches!(err, AdapterError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for line in [
            "null",
            "[]",
            "\"just a string\"",
            "1234",
            "{\"type\":123}",
            "{\"message\":{\"content\":{\"unexpected\":\"object\"}}}",
            "{\"type\":\"user\",\"timestamp\":\"not a date\"}",
            "{\"type\":\"user\",\"isSidechain\":\"not a bool\"}",
            &format!(
                "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}",
                "x".repeat(200_000)
            ),
        ] {
            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join("session.jsonl");
            std::fs::write(&path, line).expect("write");
            let _ = parse_file(&provider(), "test", &path);
        }
    }

    #[test]
    fn a_path_inside_a_record_is_never_opened() {
        // Untrusted input: a traversal string is data to record, not a path.
        let p = parse(
            r#"{"type":"user","cwd":"../../../../etc","toolUseResult":{"filePath":"../../../../etc/passwd"}}"#,
        );
        assert_eq!(p.records[0].cwd.as_deref(), Some("../../../../etc"));
        assert_eq!(
            p.records[0]
                .tool_use_result
                .as_ref()
                .unwrap()
                .touched_file(),
            Some("../../../../etc/passwd")
        );
    }
}
