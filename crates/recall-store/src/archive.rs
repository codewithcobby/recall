//! Putting sessions into `.recall/sessions/` and getting them back.
//!
//! The archive is the source of truth for conversation content. It may be the
//! only surviving copy of a conversation, so two rules shape everything here:
//! a write either completes or leaves nothing behind, and an existing archive
//! is never rewritten.
//!
//! Reading lands in #14; the failure modes in #15.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use recall_core::{FormatError, Session, SessionId};

use crate::layout::Layout;

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

        let mut writer = BufWriter::new(file);
        recall_core::write_session(&mut writer, session).map_err(|source| {
            ArchiveError::Encode {
                id: session.id.clone(),
                source,
            }
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
            .join(format!("{}.jsonl", s.id));
        assert_eq!(stored.path(), expected);
        assert!(expected.is_file());
    }

    #[test]
    fn the_archived_file_is_the_session() {
        let p = project();
        let archive = Archive::open(p.path());
        let s = session("abc");
        let stored = archive.write(&s).expect("write");

        let text = fs::read_to_string(stored.path()).expect("read");
        let back = recall_core::read_session(text.as_bytes()).expect("decode");
        assert_eq!(back, s);
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
        let text = fs::read_to_string(stored.path()).expect("read");
        assert_eq!(
            recall_core::read_session(text.as_bytes()).expect("decode"),
            s
        );
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
