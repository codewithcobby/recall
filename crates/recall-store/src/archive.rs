//! Putting sessions into `.recall/sessions/` and getting them back.
//!
//! The archive is the source of truth for conversation content. It may be the
//! only surviving copy of a conversation, so two rules shape everything here:
//! a write either completes or leaves nothing behind, and an existing archive
//! is never rewritten.
//!
//! Reading lands in #14; the failure modes in #15.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use recall_core::{FormatError, Session, SessionId};

use crate::layout::{Layout, COMPRESSION_LEVEL, SESSION_EXTENSION, UNCOMPRESSED_SESSION_EXTENSION};

/// Why an archive operation could not complete.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// A directory could not be created.
    #[error("could not create {}", .path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Writing the staged file failed.
    #[error("could not write {}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Moving the staged file into place failed.
    ///
    /// The staged file is removed, so a failed publish leaves nothing behind.
    #[error("could not move {} into {}", .from.display(), .to.display())]
    Publish {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The session could not be encoded.
    #[error("could not encode session {id}")]
    Encode {
        id: SessionId,
        #[source]
        source: FormatError,
    },

    /// No archive exists for that id.
    #[error("no archived session with id {id}")]
    NotFound { id: SessionId },

    /// The archive exists but could not be read.
    #[error("could not read the archive for session {id} at {}", .path.display())]
    Read {
        id: SessionId,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The archive exists but its contents are not a session.
    ///
    /// Never silently treated as an empty or partial session — see
    /// `.github/SECURITY.md`.
    #[error("the archive for session {id} at {} is not readable as a session", .path.display())]
    Decode {
        id: SessionId,
        path: PathBuf,
        #[source]
        source: FormatError,
    },

    /// A directory could not be listed while looking for a session.
    #[error("could not search {}", .path.display())]
    Search {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Something is at the archive's path, but it is not a file.
    #[error("the archive path for session {id} at {} is not a file", .path.display())]
    NotAFile { id: SessionId, path: PathBuf },

    /// Recall does not know how to decode this file.
    #[error("the archive for session {id} at {} has an unrecognised extension", .path.display())]
    UnknownEncoding { id: SessionId, path: PathBuf },
}

/// What a write did.
///
/// An existing archive is never rewritten, and that is an ordinary outcome
/// rather than a failure: `recall sync` runs repeatedly over the same sessions
/// and needs to say "already have it" without treating it as an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    /// The session was archived, at this path.
    Written(PathBuf),
    /// An archive for this session already existed, at this path. Untouched.
    AlreadyPresent(PathBuf),
}

impl Stored {
    /// Where the archive is, however it got there.
    pub fn path(&self) -> &Path {
        match self {
            Stored::Written(p) | Stored::AlreadyPresent(p) => p,
        }
    }

    /// Whether this call is what created it.
    pub fn is_new(&self) -> bool {
        matches!(self, Stored::Written(_))
    }
}

/// A project's session archive.
#[derive(Debug, Clone)]
pub struct Archive {
    layout: Layout,
}

impl Archive {
    /// The archive for the project rooted at `project_root`.
    ///
    /// Does not touch the filesystem — `recall init` creates the directories.
    pub fn open(project_root: impl AsRef<Path>) -> Self {
        Self {
            layout: Layout::for_project(project_root),
        }
    }

    /// The paths this archive owns.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Archive a session.
    ///
    /// Writes to `.recall/tmp` and renames into place, so an interrupted run
    /// cannot leave a half-written file that later looks like a valid archive.
    /// The staging directory sits inside `.recall/` precisely so that rename
    /// stays within one filesystem and therefore stays atomic.
    pub fn write(&self, session: &Session) -> Result<Stored, ArchiveError> {
        let destination = self
            .layout
            .session_path(session.archive_date(), &session.id);

        // Never rewrite an existing archive. Checking first is a courtesy, not
        // a guarantee — the rename below is what actually decides.
        if destination.exists() {
            return Ok(Stored::AlreadyPresent(destination));
        }

        let dir = destination
            .parent()
            .expect("session paths always have a parent")
            .to_path_buf();
        create_dir_all_owner_only(&dir)?;

        let staged = self.stage(session)?;

        // Re-check after staging: a concurrent writer may have won the race
        // while this session was being encoded.
        if destination.exists() {
            let _ = fs::remove_file(&staged);
            return Ok(Stored::AlreadyPresent(destination));
        }

        fs::rename(&staged, &destination).map_err(|source| {
            // A failed publish must not leave wreckage in tmp/.
            let _ = fs::remove_file(&staged);
            ArchiveError::Publish {
                from: staged,
                to: destination.clone(),
                source,
            }
        })?;

        Ok(Stored::Written(destination))
    }

    /// One archive found in the tree.
    #[allow(clippy::doc_markdown)]
    pub fn entries(&self) -> Result<Vec<ArchiveEntry>, ArchiveError> {
        let mut entries = Vec::new();
        for day in self.day_directories()? {
            let listing = match fs::read_dir(&day) {
                Ok(listing) => listing,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(ArchiveError::Search { path: day, source }),
            };
            for entry in listing {
                let entry = entry.map_err(|source| ArchiveError::Search {
                    path: day.clone(),
                    source,
                })?;
                let path = entry.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                // A stray file cannot be mistaken for a session: the stem has
                // to be a well-formed id and the extension one we recognise.
                let Some(id) = SessionId::parse(stem) else {
                    continue;
                };
                match path.extension().and_then(|e| e.to_str()) {
                    Some(SESSION_EXTENSION | UNCOMPRESSED_SESSION_EXTENSION) => {
                        entries.push(ArchiveEntry { id, path })
                    }
                    _ => continue,
                }
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// Read every archived session, reporting failures per session.
    ///
    /// One damaged archive must not stop a caller reading the rest — the
    /// archive may be the only copy of the others. Each result carries the id
    /// of the session it belongs to, so a failure can be reported usefully.
    pub fn read_all(&self) -> Result<Vec<Result<Session, ArchiveError>>, ArchiveError> {
        Ok(self
            .entries()?
            .into_iter()
            .map(|entry| self.read_at(&entry.id, &entry.path))
            .collect())
    }

    /// Find the archive for a session id.
    ///
    /// The id does not encode the date, so this walks the day directories and
    /// checks for a matching filename. It reads directory entries only, never
    /// file contents, so the cost is the number of day directories rather than
    /// the number of sessions. #36 replaces this with an index lookup.
    pub fn locate(&self, id: &SessionId) -> Result<Option<PathBuf>, ArchiveError> {
        for extension in [SESSION_EXTENSION, UNCOMPRESSED_SESSION_EXTENSION] {
            let name = format!("{id}.{extension}");
            for day in self.day_directories()? {
                let candidate = day.join(&name);
                match fs::symlink_metadata(&candidate) {
                    Ok(meta) if meta.is_file() => return Ok(Some(candidate)),
                    // Something is there but it is not a file. Saying "not
                    // found" would be a lie, and the difference matters when
                    // someone is trying to work out what happened to a session.
                    Ok(_) => {
                        return Err(ArchiveError::NotAFile {
                            id: id.clone(),
                            path: candidate,
                        })
                    }
                    Err(_) => continue,
                }
            }
        }
        Ok(None)
    }

    /// Read an archived session back.
    pub fn read(&self, id: &SessionId) -> Result<Session, ArchiveError> {
        let path = self
            .locate(id)?
            .ok_or_else(|| ArchiveError::NotFound { id: id.clone() })?;
        self.read_at(id, &path)
    }

    /// Read a session from a known path.
    ///
    /// The extension decides how to decode. Feeding compressed bytes to the
    /// JSON Lines reader would blame the session's contents for what is really
    /// an encoding mismatch.
    fn read_at(&self, id: &SessionId, path: &Path) -> Result<Session, ArchiveError> {
        let file = File::open(path).map_err(|source| ArchiveError::Read {
            id: id.clone(),
            path: path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);

        let decoded = match path.extension().and_then(|e| e.to_str()) {
            // Streamed, so memory scales with the largest event rather than
            // with the session.
            Some(SESSION_EXTENSION) => {
                let decoder =
                    zstd::stream::Decoder::new(reader).map_err(|source| ArchiveError::Read {
                        id: id.clone(),
                        path: path.to_path_buf(),
                        source,
                    })?;
                recall_core::read_session(BufReader::new(decoder))
            }
            Some(UNCOMPRESSED_SESSION_EXTENSION) => recall_core::read_session(reader),
            _ => {
                return Err(ArchiveError::UnknownEncoding {
                    id: id.clone(),
                    path: path.to_path_buf(),
                })
            }
        };

        decoded.map_err(|source| ArchiveError::Decode {
            id: id.clone(),
            path: path.to_path_buf(),
            source,
        })
    }

    /// Every `YYYY/MM/DD` directory holding archives, in order.
    ///
    /// Entries that are not shaped like the layout are skipped rather than
    /// erroring: `.recall/sessions/` may contain files Recall does not own, and
    /// those belong to whoever put them there.
    fn day_directories(&self) -> Result<Vec<PathBuf>, ArchiveError> {
        let mut days = Vec::new();
        for year in sorted_subdirectories(&self.layout.sessions())? {
            for month in sorted_subdirectories(&year)? {
                days.extend(sorted_subdirectories(&month)?);
            }
        }
        Ok(days)
    }

    /// Encode a session into `.recall/tmp` and return the staged path.
    fn stage(&self, session: &Session) -> Result<PathBuf, ArchiveError> {
        let tmp = self.layout.tmp();
        create_dir_all_owner_only(&tmp)?;

        // Unique per process, so two Recalls writing the same session at once
        // cannot corrupt each other's staged file.
        let staged = tmp.join(format!("{}.{}.tmp", session.id, std::process::id()));

        let file = File::create(&staged).map_err(|source| ArchiveError::Write {
            path: staged.clone(),
            source,
        })?;
        restrict_file_to_owner(&file, &staged)?;

        // Compression streams: the session is encoded an event at a time and
        // compressed as it goes, so neither step holds a whole transcript in
        // memory.
        let mut encoder = zstd::stream::Encoder::new(BufWriter::new(file), COMPRESSION_LEVEL)
            .map_err(|source| ArchiveError::Write {
                path: staged.clone(),
                source,
            })?;

        // Zstandard does not checksum frames unless asked, and without it a
        // flipped bit decompresses into whatever the corrupted bytes happen to
        // mean. That is the silent corruption SECURITY.md exists to prevent:
        // the damage would only surface if it happened to break the JSON, and
        // could otherwise be handed back as a valid session that is not the one
        // that was archived. Four bytes per archive is a cheap guarantee.
        encoder
            .include_checksum(true)
            .map_err(|source| ArchiveError::Write {
                path: staged.clone(),
                source,
            })?;
        recall_core::write_session(&mut encoder, session).map_err(|source| {
            ArchiveError::Encode {
                id: session.id.clone(),
                source,
            }
        })?;

        // finish() writes the frame terminator. Without it the archive
        // decompresses as truncated - exactly the corruption this design
        // exists to make impossible.
        let mut writer = encoder.finish().map_err(|source| ArchiveError::Write {
            path: staged.clone(),
            source,
        })?;
        writer.flush().map_err(|source| ArchiveError::Write {
            path: staged.clone(),
            source,
        })?;

        // Durability before the rename: otherwise a crash can leave the
        // directory entry pointing at a file whose contents never reached disk.
        let file = writer.into_inner().map_err(|e| ArchiveError::Write {
            path: staged.clone(),
            source: e.into_error(),
        })?;
        file.sync_all().map_err(|source| ArchiveError::Write {
            path: staged.clone(),
            source,
        })?;

        Ok(staged)
    }
}

/// An archive found in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// The session this archive holds.
    pub id: SessionId,
    /// Where it is.
    pub path: PathBuf,
}

/// Subdirectories of `dir`, sorted, or an empty list if `dir` does not exist.
fn sorted_subdirectories(dir: &Path) -> Result<Vec<PathBuf>, ArchiveError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ArchiveError::Search {
                path: dir.to_path_buf(),
                source,
            })
        }
    };

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ArchiveError::Search {
            path: dir.to_path_buf(),
            source,
        })?;
        if entry.path().is_dir() {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Create a directory and its parents, owner-readable only.
fn create_dir_all_owner_only(dir: &Path) -> Result<(), ArchiveError> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        create_dir_all_owner_only(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => restrict_dir_to_owner(dir),
        // Someone else created it between the check and the call.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(ArchiveError::CreateDirectory {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn restrict_dir_to_owner(dir: &Path) -> Result<(), ArchiveError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
        ArchiveError::CreateDirectory {
            path: dir.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_dir_to_owner(_dir: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

/// Restrict the staged file before anything is written into it.
///
/// Set on the open handle rather than the path: an archive written through a
/// world-readable staging file was never private, however briefly.
#[cfg(unix)]
fn restrict_file_to_owner(file: &File, path: &Path) -> Result<(), ArchiveError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| ArchiveError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn restrict_file_to_owner(_file: &File, _path: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use recall_core::{Provider, SessionEvent};
    use time::macros::datetime;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        crate::init(dir.path()).expect("init");
        dir
    }

    fn session(id: &str) -> Session {
        let mut s = Session::new(
            Provider::new("claude-code").expect("provider"),
            id,
            datetime!(2026-09-08 12:00:00 UTC),
        );
        s.events = vec![SessionEvent::UserMessage {
            at: None,
            content: "refactor the payment orchestrator".into(),
        }];
        s
    }

    #[test]
    fn a_session_lands_where_the_layout_says() {
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");

        let stored = archive.write(&s).expect("write");
        assert!(stored.is_new());

        let expected = p
            .path()
            .join(".recall/sessions/2026/09/08")
            .join(format!("{}.zst", s.id));
        assert_eq!(stored.path(), expected);
        assert!(expected.is_file());
    }

    #[test]
    fn the_archived_file_is_the_session() {
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");
        archive.write(&s).expect("write");
        assert_eq!(archive.read(&s.id).expect("read"), s);
    }

    #[test]
    fn an_existing_archive_is_never_rewritten() {
        // The archive may be the only copy of a conversation.
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");

        let first = archive.write(&s).expect("first write");
        assert!(first.is_new());
        let before = fs::read(first.path()).expect("read");

        // A second session with the same id but different content.
        let mut changed = session("abc");
        changed.events = vec![SessionEvent::UserMessage {
            at: None,
            content: "completely different".into(),
        }];
        assert_eq!(changed.id, s.id, "the fixture must collide by id");

        let second = archive.write(&changed).expect("second write");
        assert!(!second.is_new(), "an existing archive was rewritten");
        assert_eq!(second.path(), first.path());
        assert_eq!(
            fs::read(first.path()).expect("read"),
            before,
            "the archived bytes changed"
        );
    }

    #[test]
    fn nothing_is_left_behind_in_staging() {
        let p = project();
        let archive = Archive::open(p.path());
        archive.write(&session("abc")).expect("write");
        archive.write(&session("def")).expect("write");

        let leftovers: Vec<_> = fs::read_dir(archive.layout().tmp())
            .expect("read tmp")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "staging left {leftovers:?} behind");
    }

    #[test]
    fn sessions_are_filed_by_utc_date() {
        // 01:30 on the 9th at +05:00 is the 8th in UTC.
        let p = project();
        let archive = Archive::open(p.path());
        let s = Session::new(
            Provider::new("claude-code").expect("provider"),
            "abc",
            datetime!(2026-09-09 01:30:00 +05:00),
        );
        let stored = archive.write(&s).expect("write");
        assert!(
            stored
                .path()
                .starts_with(p.path().join(".recall/sessions/2026/09/08")),
            "landed at {}",
            stored.path().display()
        );
    }

    #[test]
    fn two_sessions_on_the_same_day_share_a_directory() {
        let p = project();
        let archive = Archive::open(p.path());
        let a = archive.write(&session("abc")).expect("write a");
        let b = archive.write(&session("def")).expect("write b");
        assert_ne!(a.path(), b.path());
        assert_eq!(a.path().parent(), b.path().parent());
    }

    #[test]
    fn a_large_session_survives_the_round_trip() {
        let p = project();
        let archive = Archive::open(p.path());
        let mut s = session("big");
        s.events = (0..2_000)
            .map(|i| SessionEvent::UserMessage {
                at: None,
                content: format!("message {i} {}", "x".repeat(500)),
            })
            .collect();

        let stored = archive.write(&s).expect("write");
        assert_eq!(archive.read(&s.id).expect("read"), s);

        // And it is genuinely compressed: this session is highly repetitive,
        // so the archive should be a small fraction of the encoded size.
        let mut raw = Vec::new();
        recall_core::write_session(&mut raw, &s).expect("encode");
        let on_disk = fs::metadata(stored.path()).expect("metadata").len() as usize;
        assert!(
            on_disk * 4 < raw.len(),
            "archive is {on_disk} bytes for {} encoded - barely compressed",
            raw.len()
        );
    }

    #[test]
    fn a_session_reads_back_exactly_as_written() {
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");
        archive.write(&s).expect("write");

        assert_eq!(archive.read(&s.id).expect("read"), s);
    }

    #[test]
    fn every_event_variant_survives_the_archive() {
        use recall_core::FileAction;
        let p = project();
        let archive = Archive::open(p.path());
        let mut s = session("abc");
        s.model = Some("claude-opus-5".into());
        s.ended_at = Some(datetime!(2026-09-08 14:30:00 UTC));
        s.project = Some("/home/me/project".into());
        s.events = vec![
            SessionEvent::UserMessage {
                at: Some(datetime!(2026-09-08 12:00:05 UTC)),
                content: "line one\nline two".into(),
            },
            SessionEvent::AssistantMessage {
                at: None,
                content: "unicode 日本語 🧠".into(),
            },
            SessionEvent::ToolCall {
                at: None,
                name: "read_file".into(),
                arguments: Some(r#"{"path":"src/lib.rs"}"#.into()),
                call_id: Some("call-1".into()),
            },
            SessionEvent::ToolResult {
                at: None,
                call_id: Some("call-1".into()),
                content: "x".repeat(100_000),
                failed: Some(false),
            },
            SessionEvent::Command {
                at: None,
                command: "cargo test".into(),
                exit_code: Some(0),
                output: None,
            },
            SessionEvent::FileChange {
                at: None,
                action: FileAction::Deleted,
                path: "../../../../etc/passwd".into(),
            },
            SessionEvent::Other {
                at: None,
                provider_kind: "system_prompt".into(),
                content: "be concise".into(),
            },
        ];
        archive.write(&s).expect("write");
        assert_eq!(archive.read(&s.id).expect("read"), s);
    }

    #[test]
    fn a_session_is_found_whatever_day_it_was_filed_under() {
        let p = project();
        let archive = Archive::open(p.path());

        // Spread across years, months and days so the walk has to work.
        let dates = [
            datetime!(2024-01-01 00:00:00 UTC),
            datetime!(2025-06-15 12:00:00 UTC),
            datetime!(2026-09-08 23:59:59 UTC),
        ];
        let mut written = Vec::new();
        for (i, at) in dates.iter().enumerate() {
            let s = Session::new(
                Provider::new("claude-code").expect("provider"),
                format!("session-{i}"),
                *at,
            );
            archive.write(&s).expect("write");
            written.push(s);
        }

        for s in &written {
            assert_eq!(archive.read(&s.id).expect("read").id, s.id);
        }
    }

    #[test]
    fn an_unknown_id_is_reported_as_missing() {
        let p = project();
        let archive = Archive::open(p.path());
        archive.write(&session("abc")).expect("write");

        let absent = session("never-archived").id;
        assert!(archive.locate(&absent).expect("locate").is_none());
        assert!(matches!(
            archive.read(&absent),
            Err(ArchiveError::NotFound { .. })
        ));
    }

    #[test]
    fn an_empty_archive_is_searchable_without_error() {
        let p = project();
        let archive = Archive::open(p.path());
        assert!(archive
            .locate(&session("abc").id)
            .expect("locate")
            .is_none());
    }

    #[test]
    fn locate_reads_directory_entries_not_archives() {
        // The cost is the number of day directories, not the number of
        // sessions. A thousand sessions in one day must not mean a thousand
        // file reads to find one.
        let p = project();
        let archive = Archive::open(p.path());
        for i in 0..200 {
            archive.write(&session(&format!("s{i}"))).expect("write");
        }
        let target = session("s150");
        let found = archive
            .locate(&target.id)
            .expect("locate")
            .expect("present");
        assert!(found.ends_with(format!("{}.zst", target.id)));
    }

    #[test]
    fn files_recall_does_not_own_are_ignored_while_searching() {
        // .recall/sessions may contain things Recall did not put there.
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");
        archive.write(&s).expect("write");

        fs::write(archive.layout().sessions().join("notes.md"), b"mine").expect("write");
        fs::create_dir_all(archive.layout().sessions().join("scratch/deep")).expect("mkdir");

        assert_eq!(archive.read(&s.id).expect("read"), s);
    }

    #[test]
    fn archives_are_zstandard_frames() {
        let p = project();
        let archive = Archive::open(p.path());
        let stored = archive.write(&session("abc")).expect("write");

        let bytes = fs::read(stored.path()).expect("read");
        // The zstd magic number. If this ever changes, the extension is lying
        // about the encoding and readers will guess wrong.
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "not a zstd frame");
    }

    #[test]
    fn a_flipped_bit_is_caught_rather_than_decoded() {
        // Zstandard does not checksum frames unless asked. Without that, a
        // corrupted archive decompresses into whatever the damaged bytes happen
        // to mean, and only surfaces if the result also breaks the JSON - which
        // it need not. This asserts the checksum is on.
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");
        let stored = archive.write(&s).expect("write");

        let mut bytes = fs::read(stored.path()).expect("read");
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x01;
        fs::write(stored.path(), &bytes).expect("corrupt");

        match archive.read(&s.id) {
            Err(ArchiveError::Decode { id, .. }) => assert_eq!(id, s.id),
            Ok(recovered) => panic!(
                "a corrupted archive was accepted as a session with {} events",
                recovered.event_count()
            ),
            other => panic!("expected a decode error, got {other:?}"),
        }
    }

    #[test]
    fn compression_is_worth_having() {
        // Not a benchmark, just a floor: a transcript with the repetition real
        // sessions have should shrink substantially.
        let p = project();
        let archive = Archive::open(p.path());
        let mut s = session("abc");
        s.events = (0..300)
            .map(|i| SessionEvent::AssistantMessage {
                at: None,
                content: format!(
                    "Looking at the retry path in step {i}. The current implementation \
                     uses a fixed delay, which means a downstream outage produces a \
                     thundering herd rather than backing off."
                ),
            })
            .collect();

        let stored = archive.write(&s).expect("write");
        let mut encoded = Vec::new();
        recall_core::write_session(&mut encoded, &s).expect("encode");
        let on_disk = fs::metadata(stored.path()).expect("metadata").len() as usize;

        assert!(
            on_disk * 5 < encoded.len(),
            "{on_disk} bytes on disk for {} encoded - compression is not working",
            encoded.len()
        );
        assert_eq!(archive.read(&s.id).expect("read"), s);
    }

    #[cfg(unix)]
    #[test]
    fn archives_are_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let p = project();
        let archive = Archive::open(p.path());
        let stored = archive.write(&session("abc")).expect("write");

        let mode = fs::metadata(stored.path())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "archive is {mode:o}, expected 600");

        // Including every directory created on the way down.
        let mut dir = stored.path().parent();
        while let Some(d) = dir {
            if !d.starts_with(p.path().join(".recall")) {
                break;
            }
            let mode = fs::metadata(d).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}, expected 700", d.display());
            dir = d.parent();
        }
    }
}

/// Every way an archive can be broken, and what Recall does about it.
///
/// The rule these all share: an error naming the session, never a panic, and
/// never a partial or empty session handed back as though it were real.
#[cfg(test)]
mod damage_tests {
    use super::*;
    use recall_core::{Provider, SessionEvent};
    use time::macros::datetime;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        crate::init(dir.path()).expect("init");
        dir
    }

    fn session(id: &str) -> Session {
        let mut s = Session::new(
            Provider::new("claude-code").expect("provider"),
            id,
            datetime!(2026-09-08 12:00:00 UTC),
        );
        s.events = vec![SessionEvent::UserMessage {
            at: None,
            content: format!("session {id}"),
        }];
        s
    }

    /// Archive a session and hand back its path, ready to be damaged.
    fn archived(archive: &Archive, id: &str) -> (Session, PathBuf) {
        let s = session(id);
        let stored = archive.write(&s).expect("write");
        (s, stored.path().to_path_buf())
    }

    #[test]
    fn a_missing_archive_is_reported_as_missing() {
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");
        fs::remove_file(&path).expect("remove");

        assert!(matches!(
            archive.read(&s.id),
            Err(ArchiveError::NotFound { .. })
        ));
    }

    #[test]
    fn a_truncated_archive_is_refused() {
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");

        let bytes = fs::read(&path).expect("read");
        fs::write(&path, &bytes[..bytes.len() / 2]).expect("truncate");

        match archive.read(&s.id) {
            Err(ArchiveError::Decode { id, .. }) => assert_eq!(id, s.id),
            other => panic!("expected a decode error naming the session, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_archive_file_is_not_an_empty_session() {
        // The failure mode SECURITY.md calls out by name.
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");
        fs::write(&path, b"").expect("empty it");

        match archive.read(&s.id) {
            Err(ArchiveError::Decode { id, .. }) => assert_eq!(id, s.id),
            Ok(session) => panic!(
                "an empty file was read as a session with {} events",
                session.event_count()
            ),
            other => panic!("expected a decode error, got {other:?}"),
        }
    }

    #[test]
    fn garbage_bytes_are_refused() {
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");
        fs::write(&path, [0xff, 0x00, 0xfe, 0x42, 0x00, 0x99]).expect("garbage");

        assert!(matches!(
            archive.read(&s.id),
            Err(ArchiveError::Decode { .. })
        ));
    }

    #[test]
    fn an_unknown_format_version_is_refused() {
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");

        let raw = zstd::decode_all(fs::File::open(&path).expect("open")).expect("decompress");
        let patched =
            String::from_utf8(raw)
                .expect("utf8")
                .replacen("\"format\":1", "\"format\":99", 1);
        fs::write(
            &path,
            zstd::encode_all(patched.as_bytes(), 1).expect("recompress"),
        )
        .expect("write");

        match archive.read(&s.id) {
            Err(ArchiveError::Decode { id, .. }) => assert_eq!(id, s.id),
            other => panic!("expected a decode error, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_where_the_archive_belongs_is_reported_as_such() {
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");
        fs::remove_file(&path).expect("remove");
        fs::create_dir(&path).expect("directory in its place");

        match archive.read(&s.id) {
            Err(ArchiveError::NotAFile { id, .. }) => assert_eq!(id, s.id),
            other => panic!("expected a not-a-file error, got {other:?}"),
        }
    }

    #[test]
    fn an_uncompressed_archive_from_an_earlier_build_still_reads() {
        // Naming the encoding in the extension is what lets compression land
        // without a migration. An archive written before #16 must still open.
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");

        // Write it the old way: plain JSON Lines under .jsonl.
        let dir = archive.layout().session_dir(s.archive_date());
        fs::create_dir_all(&dir).expect("create day directory");
        let legacy = dir.join(format!("{}.jsonl", s.id));
        let mut bytes = Vec::new();
        recall_core::write_session(&mut bytes, &s).expect("encode");
        fs::write(&legacy, &bytes).expect("write");

        assert_eq!(archive.read(&s.id).expect("read"), s);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_archive_is_reported_rather_than_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");

        // Root ignores permission bits, so the test would prove nothing there.
        // Detect that by trying, rather than by asking who we are.
        if File::open(&path).is_ok() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore");
            return;
        }

        let result = archive.read(&s.id);
        // Restore before asserting, so a failure does not leave an
        // undeletable temp directory behind.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore");

        match result {
            Err(ArchiveError::Read { id, .. }) => assert_eq!(id, s.id),
            other => panic!("expected a read error naming the session, got {other:?}"),
        }
    }

    #[test]
    fn one_damaged_archive_does_not_hide_the_others() {
        // The whole point: the archive may be the only copy of the sessions
        // that are still fine.
        let p = project();
        let archive = Archive::open(p.path());

        let (good_one, _) = archived(&archive, "one");
        let (broken, broken_path) = archived(&archive, "two");
        let (good_two, _) = archived(&archive, "three");
        fs::write(&broken_path, b"{not json at all").expect("damage");

        let results = archive.read_all().expect("enumerate");
        assert_eq!(results.len(), 3, "an archive went missing from the listing");

        let recovered: Vec<_> = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|s| s.id.clone())
            .collect();
        assert!(recovered.contains(&good_one.id));
        assert!(recovered.contains(&good_two.id));
        assert!(!recovered.contains(&broken.id));

        let failures: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
        assert_eq!(failures.len(), 1);
        assert!(
            format!("{}", failures[0]).contains(broken.id.as_str()),
            "the failure did not name the session: {}",
            failures[0]
        );
    }

    #[test]
    fn stray_files_in_the_archive_are_not_mistaken_for_sessions() {
        let p = project();
        let archive = Archive::open(p.path());
        let (s, path) = archived(&archive, "abc");
        let day = path.parent().expect("day directory");

        fs::write(day.join("notes.md"), b"mine").expect("write");
        fs::write(day.join("not-an-id.jsonl"), b"{}").expect("write");
        fs::write(day.join(format!("{}.bak", s.id)), b"{}").expect("write");

        let entries = archive.entries().expect("entries");
        assert_eq!(entries.len(), 1, "found {entries:?}");
        assert_eq!(entries[0].id, s.id);
    }

    #[test]
    fn nothing_here_panics() {
        // Every damaged shape in one pass, asserting only that each returns
        // rather than unwinding.
        let p = project();
        let archive = Archive::open(p.path());
        for (i, damage) in [
            b"".to_vec(),
            b"\x00\x01\x02".to_vec(),
            b"{".to_vec(),
            b"[]".to_vec(),
            b"null".to_vec(),
            "\u{feff}{}".as_bytes().to_vec(),
            b"{\"format\":1}".to_vec(),
            vec![b'x'; 1_000_000],
        ]
        .into_iter()
        .enumerate()
        {
            let (s, path) = archived(&archive, &format!("case-{i}"));
            fs::write(&path, &damage).expect("damage");
            let _ = archive.read(&s.id);
        }
    }
}
