//! Reading a Codex rollout into [`Record`]s.
//!
//! Line-oriented and forgiving, for the same reason the Claude Code parser is:
//! a rollout is untrusted input written by software this project does not
//! control, so a line that cannot be understood costs that line rather than the
//! session. A rollout being written *right now* is the ordinary case — its last
//! line is usually half-written.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use recall_core::{AdapterError, Provider};

use super::record::Record;

/// What one rollout contained.
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

/// Read one rollout.
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
    use crate::codex::record::{Item, KnownItem};

    fn provider() -> Provider {
        Provider::new("codex").expect("provider")
    }

    fn parse(lines: &str) -> ParsedFile {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, lines).expect("write");
        parse_file(&provider(), "test", &path).expect("parse")
    }

    #[test]
    fn the_envelope_is_read_off_every_record() {
        let p = parse(
            r#"{"timestamp":"2026-08-03T13:11:04.855Z","ordinal":0,"type":"session_meta","payload":{"session_id":"s1"}}"#,
        );
        assert_eq!(p.records.len(), 1);
        let r = &p.records[0];
        assert_eq!(r.kind.as_deref(), Some("session_meta"));
        assert_eq!(r.timestamp.as_deref(), Some("2026-08-03T13:11:04.855Z"));
        assert_eq!(r.ordinal, Some(0));
    }

    #[test]
    fn a_truncated_final_line_costs_that_line_only() {
        // The ordinary case: the rollout is still being written.
        let p = parse(concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"mess"#,
        ));
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.unreadable_lines, 1);
        assert!(matches!(
            p.records[0].response_item(),
            Some(Item::Known(KnownItem::Message { .. }))
        ));
    }

    #[test]
    fn blank_lines_are_not_records_and_are_not_failures() {
        let p = parse("\n\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\"}}\n\n");
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.unreadable_lines, 0);
    }

    #[test]
    fn an_empty_file_reads_as_empty_rather_than_failing() {
        // Nothing was lost — there was nothing there. That is different from a
        // file whose every line failed, and the caller decides what to do.
        let p = parse("");
        assert!(p.is_empty());
        assert_eq!(p.unreadable_lines, 0);
    }

    #[test]
    fn a_file_with_nothing_readable_in_it_is_reported_as_malformed() {
        // Archiving this as an empty session would claim the conversation was
        // empty rather than lost.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, "not json\nnor this\n").expect("write");

        let err = parse_file(&provider(), "s1", &path).expect_err("should be malformed");
        assert!(matches!(err, AdapterError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_file_is_an_io_error_not_a_parse_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let err =
            parse_file(&provider(), "s1", &dir.path().join("absent.jsonl")).expect_err("missing");
        assert!(matches!(err, AdapterError::Io { .. }), "{err:?}");
    }

    #[test]
    fn a_record_with_no_payload_is_still_a_record() {
        let p = parse(r#"{"type":"world_state"}"#);
        assert_eq!(p.records.len(), 1);
        assert!(p.records[0].payload.is_none());
    }

    #[test]
    fn parsing_does_not_modify_the_rollout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let contents = "{\"type\":\"world_state\"}\n";
        std::fs::write(&path, contents).expect("write");
        let modified_before = std::fs::metadata(&path).expect("metadata").modified().ok();

        parse_file(&provider(), "s1", &path).expect("parse");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").modified().ok(),
            modified_before,
            "parsing modified the provider's file"
        );
    }
}
