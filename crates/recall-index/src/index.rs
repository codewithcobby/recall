//! The index itself: opening it, writing to it, reading it back.
//!
//! Everything here is derived from the archives. Nothing in this file is the
//! only copy of anything, which is what licenses the recovery behaviour: when
//! the database is unreadable or was written by a newer build, it is discarded
//! and rebuilt rather than migrated or repaired.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use recall_core::{GitContext, Provider, Session, SessionHeader, SessionId};
use rusqlite::{Connection, OptionalExtension};

use crate::schema::{self, SCHEMA, SCHEMA_VERSION};
use crate::timestamp;

/// Why an index operation could not complete.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The database file could not be created.
    #[error("could not create the index at {}", .path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// SQLite would not open the file.
    #[error("could not open the index at {}", .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// The schema could not be created.
    #[error("could not create the index schema")]
    Schema {
        #[source]
        source: rusqlite::Error,
    },

    /// A write failed.
    #[error("could not write to the index")]
    Write {
        #[source]
        source: rusqlite::Error,
    },

    /// A read failed.
    #[error("could not read the index")]
    Read {
        #[source]
        source: rusqlite::Error,
    },

    /// A stored row does not decode into a session.
    ///
    /// Means the database disagrees with this build about what it holds. The
    /// index is derived, so the answer is to rebuild it, never to guess at what
    /// the row meant.
    #[error("the index holds a row that is not a session: {what}")]
    Malformed { what: String },

    /// A path could not be stored as text.
    ///
    /// Reported rather than stored lossily. A path that round-trips through
    /// `to_string_lossy` is a different path, and one that silently points
    /// somewhere else is worse than one that is missing — the archive still
    /// holds the truth either way.
    #[error("{} is not valid UTF-8, so it cannot be indexed", .path.display())]
    PathNotUtf8 { path: PathBuf },

    /// A timestamp could not be written in the canonical form.
    #[error("could not encode the timestamp for session {id}")]
    Timestamp {
        id: SessionId,
        #[source]
        source: time::error::Format,
    },
}

/// One session as the index knows it.
///
/// Metadata and a pointer, never conversation content — the boundary #37
/// documents and tests.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedSession {
    /// The session's metadata. Its `events` are always empty here.
    pub session: Session,
    /// How many events the archive holds, when it records the count.
    pub event_count: Option<usize>,
    /// Where the archive is, relative to `.recall/`.
    ///
    /// Relative so that moving or copying a project does not invalidate every
    /// row, and stored rather than derived because the extension depends on
    /// when the archive was written.
    pub archive_path: PathBuf,
}

impl IndexedSession {
    /// Build a row from what a listing of the archive already produces.
    pub fn new(header: SessionHeader, archive_path: impl Into<PathBuf>) -> Self {
        Self {
            session: header.session,
            event_count: header.event_count,
            archive_path: archive_path.into(),
        }
    }
}

/// Why an existing database could not be used as it stood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// It opened and its schema is the one this build wrote.
    No,
    /// It was unreadable, or not a database at all.
    Unreadable,
    /// Its schema version is not the one this build understands.
    SchemaVersion { found: u32 },
}

/// What opening the index did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// Whether the file had to be discarded and recreated, and why.
    pub recovered: Recovered,
    /// Whether there was no database before this call.
    pub created: bool,
}

/// The metadata index for one project.
pub struct Index {
    connection: Connection,
    path: PathBuf,
}

impl Index {
    /// Open the index at `path`, creating it if it is not there.
    ///
    /// A database that cannot be opened, or that carries a schema version this
    /// build does not understand, is **replaced**. That is safe precisely
    /// because nothing here is a source of truth: the caller rebuilds from the
    /// archives, and the alternative — refusing to run until the user deletes a
    /// file by hand — fails a command that has everything it needs to succeed.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Opened), IndexError> {
        let path = path.as_ref().to_path_buf();
        let existed = path.exists();

        match Self::open_existing(&path) {
            Ok(index) => Ok((
                index,
                Opened {
                    recovered: Recovered::No,
                    created: !existed,
                },
            )),
            Err((recovered, _)) => {
                Self::discard(&path)?;
                // The second attempt is against a path with nothing at it. If
                // that fails the problem is the directory or the disk, not the
                // database, so its own error is the one worth reporting —
                // retrying again would only loop.
                let index = Self::open_existing(&path).map_err(|(_, why)| why)?;
                Ok((
                    index,
                    Opened {
                        recovered,
                        created: true,
                    },
                ))
            }
        }
    }

    /// Open, or say both why the file has to go and what actually went wrong.
    fn open_existing(path: &Path) -> Result<Self, (Recovered, IndexError)> {
        let unreadable = |source: rusqlite::Error| {
            (
                Recovered::Unreadable,
                IndexError::Open {
                    path: path.to_path_buf(),
                    source,
                },
            )
        };

        create_private(path).map_err(|source| {
            (
                Recovered::Unreadable,
                IndexError::Create {
                    path: path.to_path_buf(),
                    source,
                },
            )
        })?;

        let connection = Connection::open(path).map_err(unreadable)?;
        // The first statement to touch the file header. A file that is not a
        // database fails here rather than at the first query.
        configure(&connection).map_err(unreadable)?;

        let version = schema::version(&connection).map_err(unreadable)?;
        match version {
            0 => {
                // Either brand new, or a database that is not ours. Creating
                // the tables is what tells those apart: it fails if they exist.
                connection
                    .execute_batch(SCHEMA)
                    .map_err(|source| (Recovered::Unreadable, IndexError::Schema { source }))?;
                schema::set_version(&connection, SCHEMA_VERSION)
                    .map_err(|source| (Recovered::Unreadable, IndexError::Schema { source }))?;
            }
            v if v == SCHEMA_VERSION => {}
            found => {
                return Err((
                    Recovered::SchemaVersion { found },
                    IndexError::Malformed {
                        what: format!(
                            "schema version {found}, but this build understands {SCHEMA_VERSION}"
                        ),
                    },
                ))
            }
        }

        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    /// Remove a database that cannot be used, and its sidecar files.
    fn discard(path: &Path) -> Result<(), IndexError> {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let sidecar = PathBuf::from(name);
            match fs::remove_file(&sidecar) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(IndexError::Create {
                        path: sidecar,
                        source,
                    })
                }
            }
        }
        Ok(())
    }

    /// Where the database is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert or update rows, in one transaction.
    ///
    /// Batched deliberately: a transaction per session turns an import of a few
    /// hundred sessions into a few hundred fsyncs. Either every row in the
    /// batch lands or none does, so a failure never leaves the index holding
    /// half of a sync.
    pub fn upsert(&mut self, sessions: &[IndexedSession]) -> Result<usize, IndexError> {
        if sessions.is_empty() {
            return Ok(0);
        }

        let transaction = self
            .connection
            .transaction()
            .map_err(|source| IndexError::Write { source })?;
        insert_batch(&transaction, sessions)?;
        transaction
            .commit()
            .map_err(|source| IndexError::Write { source })?;
        Ok(sessions.len())
    }

    /// Every session, newest first.
    pub fn list(&self) -> Result<Vec<IndexedSession>, IndexError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, provider, provider_session_id, model,
                        started_at, ended_at, project, repository, branch,
                        commit_at_start, commit_at_end, event_count, archive_path
                 FROM sessions
                 ORDER BY started_at DESC",
            )
            .map_err(|source| IndexError::Read { source })?;

        let rows = statement
            .query_map([], Row::from_row)
            .map_err(|source| IndexError::Read { source })?;

        let mut out = Vec::new();
        for row in rows {
            let row = row.map_err(|source| IndexError::Read { source })?;
            out.push(row.into_session()?);
        }
        Ok(out)
    }

    /// Whether a session is already indexed.
    pub fn contains(&self, id: &SessionId) -> Result<bool, IndexError> {
        let found: Option<i64> = self
            .connection
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| IndexError::Read { source })?;
        Ok(found.is_some())
    }

    /// How many sessions the index holds.
    pub fn count(&self) -> Result<usize, IndexError> {
        let n: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(|source| IndexError::Read { source })?;
        Ok(n.max(0) as usize)
    }

    /// Forget everything.
    ///
    /// Used when rebuilding: the archives are re-read and the index refilled.
    pub fn clear(&mut self) -> Result<(), IndexError> {
        self.connection
            .execute("DELETE FROM sessions", [])
            .map_err(|source| IndexError::Write { source })?;
        Ok(())
    }

    /// Replace the index's contents with exactly these sessions.
    ///
    /// One transaction, so a rebuild that fails partway leaves the previous
    /// contents rather than an empty index.
    pub fn replace_all(&mut self, sessions: &[IndexedSession]) -> Result<usize, IndexError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|source| IndexError::Write { source })?;
        transaction
            .execute("DELETE FROM sessions", [])
            .map_err(|source| IndexError::Write { source })?;
        insert_batch(&transaction, sessions)?;
        transaction
            .commit()
            .map_err(|source| IndexError::Write { source })?;
        Ok(sessions.len())
    }
}

/// Write rows inside an open transaction.
///
/// Shared by `upsert` and `replace_all` so that a rebuild is one atomic
/// swap — the old contents survive a rebuild that fails partway, rather than
/// leaving an index that has been emptied but not refilled.
fn insert_batch(
    transaction: &rusqlite::Transaction<'_>,
    sessions: &[IndexedSession],
) -> Result<(), IndexError> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO sessions (
                 id, provider, provider_session_id, model,
                 started_at, ended_at, project, repository, branch,
                 commit_at_start, commit_at_end, event_count, archive_path
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                 model            = excluded.model,
                 started_at       = excluded.started_at,
                 ended_at         = excluded.ended_at,
                 project          = excluded.project,
                 repository       = excluded.repository,
                 branch           = excluded.branch,
                 commit_at_start  = excluded.commit_at_start,
                 commit_at_end    = excluded.commit_at_end,
                 event_count      = excluded.event_count,
                 archive_path     = excluded.archive_path",
        )
        .map_err(|source| IndexError::Write { source })?;

    for indexed in sessions {
        let s = &indexed.session;
        let git = s.git.clone().unwrap_or_default();
        let started_at =
            timestamp::encode(s.started_at).map_err(|source| IndexError::Timestamp {
                id: s.id.clone(),
                source,
            })?;
        let ended_at = s
            .ended_at
            .map(timestamp::encode)
            .transpose()
            .map_err(|source| IndexError::Timestamp {
                id: s.id.clone(),
                source,
            })?;

        statement
            .execute(rusqlite::params![
                s.id.as_str(),
                s.provider.as_str(),
                s.provider_session_id,
                s.model,
                started_at,
                ended_at,
                as_text(s.project.as_deref())?,
                as_text(git.repository.as_deref())?,
                git.branch,
                git.commit_at_start,
                git.commit_at_end,
                indexed.event_count.map(|n| n as i64),
                as_text(Some(indexed.archive_path.as_path()))?,
            ])
            .map_err(|source| IndexError::Write { source })?;
    }
    Ok(())
}

/// Configure a freshly opened connection.
fn configure(connection: &Connection) -> rusqlite::Result<()> {
    // WAL so a long read does not block the sync that is writing, and so an
    // interrupted write leaves a recoverable file. `Layout::owns` already
    // accounts for the -wal and -shm sidecars.
    //
    // NORMAL rather than FULL: losing the last transaction to a power cut costs
    // a rebuild, not data, and the archives are fsynced independently.
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// Create the file private before SQLite ever opens it.
///
/// Order matters: creating it and chmod-ing afterwards leaves a window in which
/// the database is world-readable. `.recall/` is 0700, so this is defence in
/// depth rather than the only barrier — see `.github/SECURITY.md`.
fn create_private(path: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    Ok(())
}

/// A path as text, or a clear error.
fn as_text(path: Option<&Path>) -> Result<Option<String>, IndexError> {
    match path {
        None => Ok(None),
        Some(p) => p
            .to_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| IndexError::PathNotUtf8 {
                path: p.to_path_buf(),
            }),
    }
}

/// One row, straight out of SQLite.
struct Row {
    id: String,
    provider: String,
    provider_session_id: String,
    model: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    project: Option<String>,
    repository: Option<String>,
    branch: Option<String>,
    commit_at_start: Option<String>,
    commit_at_end: Option<String>,
    event_count: Option<i64>,
    archive_path: String,
}

impl Row {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            provider: row.get(1)?,
            provider_session_id: row.get(2)?,
            model: row.get(3)?,
            started_at: row.get(4)?,
            ended_at: row.get(5)?,
            project: row.get(6)?,
            repository: row.get(7)?,
            branch: row.get(8)?,
            commit_at_start: row.get(9)?,
            commit_at_end: row.get(10)?,
            event_count: row.get(11)?,
            archive_path: row.get(12)?,
        })
    }

    /// Turn a row back into a session, refusing anything that does not fit.
    fn into_session(self) -> Result<IndexedSession, IndexError> {
        let id = SessionId::parse(&self.id).ok_or_else(|| IndexError::Malformed {
            what: format!("{:?} is not a session id", self.id),
        })?;
        let provider = Provider::new(self.provider.clone()).map_err(|e| IndexError::Malformed {
            what: format!("provider {:?}: {e}", self.provider),
        })?;
        let started_at =
            timestamp::decode(&self.started_at).map_err(|e| IndexError::Malformed {
                what: format!("started_at {:?}: {e}", self.started_at),
            })?;
        let ended_at = self
            .ended_at
            .as_deref()
            .map(timestamp::decode)
            .transpose()
            .map_err(|e| IndexError::Malformed {
                what: format!("ended_at: {e}"),
            })?;

        // The id is derived from the provider and the provider's id. A row
        // where they disagree was not written by this code, and trusting either
        // half would attach one session's metadata to another's archive.
        let derived = SessionId::derive(&provider, &self.provider_session_id);
        if derived != id {
            return Err(IndexError::Malformed {
                what: format!("session {id} does not match its provider fields"),
            });
        }

        let git = GitContext {
            repository: self.repository.map(PathBuf::from),
            branch: self.branch,
            commit_at_start: self.commit_at_start,
            commit_at_end: self.commit_at_end,
        };

        let mut session = Session::new(provider, self.provider_session_id, started_at);
        session.model = self.model;
        session.ended_at = ended_at;
        session.project = self.project.map(PathBuf::from);
        session.git = (!git.is_empty()).then_some(git);

        Ok(IndexedSession {
            session,
            event_count: self.event_count.map(|n| n.max(0) as usize),
            archive_path: PathBuf::from(self.archive_path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recall_core::SessionEvent;
    use time::macros::datetime;
    use time::OffsetDateTime;

    fn provider() -> Provider {
        Provider::new("claude-code").expect("provider")
    }

    /// An index in a directory that lasts as long as the test.
    fn index() -> (tempfile::TempDir, Index) {
        let dir = tempfile::tempdir().expect("temp dir");
        let (index, _) = Index::open(dir.path().join("index.db")).expect("open");
        (dir, index)
    }

    /// A session carrying something in every column.
    fn full_session(id: &str, started_at: OffsetDateTime) -> IndexedSession {
        let mut session = Session::new(provider(), id, started_at);
        session.model = Some("claude-opus-5".into());
        session.ended_at = Some(started_at + time::Duration::minutes(30));
        session.project = Some(PathBuf::from("/w/demo"));
        session.git = Some(GitContext {
            repository: Some(PathBuf::from("/w/demo")),
            branch: Some("feature/34-sqlite-index-schema".into()),
            commit_at_start: None,
            commit_at_end: None,
        });
        IndexedSession {
            session,
            event_count: Some(42),
            archive_path: PathBuf::from("sessions/2026/09/08/abc.zst"),
        }
    }

    /// The barest session a provider could produce.
    fn bare_session(id: &str, started_at: OffsetDateTime) -> IndexedSession {
        IndexedSession {
            session: Session::new(provider(), id, started_at),
            event_count: None,
            archive_path: PathBuf::from("sessions/2026/09/08/bare.zst"),
        }
    }

    #[test]
    fn a_new_index_is_created_with_a_versioned_schema() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("index.db");
        let (index, opened) = Index::open(&path).expect("open");

        assert!(path.is_file(), "no database was created");
        assert!(opened.created);
        assert_eq!(opened.recovered, Recovered::No);
        assert_eq!(
            schema::version(&index.connection).expect("version"),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn reopening_an_index_leaves_it_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("index.db");

        let (mut first, _) = Index::open(&path).expect("open");
        first
            .upsert(&[bare_session("s", datetime!(2026-09-08 12:00:00 UTC))])
            .expect("upsert");
        drop(first);

        let (second, opened) = Index::open(&path).expect("reopen");
        assert!(!opened.created, "an existing index was recreated");
        assert_eq!(opened.recovered, Recovered::No);
        assert_eq!(second.count().expect("count"), 1, "the row was lost");
    }

    #[cfg(unix)]
    #[test]
    fn the_database_is_private() {
        // Same rule as the archive: nothing Recall writes is readable by other
        // users on the machine. See .github/SECURITY.md.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("index.db");
        let _ = Index::open(&path).expect("open");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "index.db is {mode:o}, expected 600");
    }

    #[test]
    fn everything_written_comes_back_unchanged() {
        let (_dir, mut index) = index();
        let original = full_session("s", datetime!(2026-09-08 12:00:00.123456789 UTC));

        assert_eq!(index.upsert(std::slice::from_ref(&original)).unwrap(), 1);

        let listed = index.list().expect("list");
        assert_eq!(listed, vec![original]);
    }

    #[test]
    fn absence_survives_the_round_trip() {
        // A field the provider could not supply must come back missing, not as
        // an empty string. Guessing here would make a session claim more than
        // its source contained.
        let (_dir, mut index) = index();
        let bare = bare_session("s", datetime!(2026-09-08 12:00:00 UTC));
        index.upsert(std::slice::from_ref(&bare)).expect("upsert");

        let back = index.list().expect("list").remove(0);
        assert_eq!(back.session.model, None);
        assert_eq!(back.session.ended_at, None);
        assert_eq!(back.session.project, None);
        assert_eq!(back.session.git, None);
        assert_eq!(back.event_count, None);
        assert_eq!(back, bare);
    }

    #[test]
    fn sessions_come_back_newest_first() {
        let (_dir, mut index) = index();
        // Inserted oldest-first, so the order cannot come from insertion order.
        index
            .upsert(&[
                bare_session("oldest", datetime!(2025-01-01 00:00:00 UTC)),
                bare_session("middle", datetime!(2026-09-08 12:00:00 UTC)),
                bare_session("newest", datetime!(2026-09-08 12:00:00.5 UTC)),
            ])
            .expect("upsert");

        let ids: Vec<String> = index
            .list()
            .expect("list")
            .into_iter()
            .map(|s| s.session.provider_session_id)
            .collect();
        assert_eq!(ids, ["newest", "middle", "oldest"]);
    }

    #[test]
    fn indexing_the_same_session_twice_updates_it() {
        // Sync runs repeatedly over the same sessions. A second pass must
        // correct a row, not duplicate it.
        let (_dir, mut index) = index();
        let at = datetime!(2026-09-08 12:00:00 UTC);

        index.upsert(&[bare_session("s", at)]).expect("first");
        let mut revised = full_session("s", at);
        revised.session.provider_session_id = "s".into();
        revised.session.id = SessionId::derive(&provider(), "s");
        index
            .upsert(std::slice::from_ref(&revised))
            .expect("second");

        assert_eq!(index.count().expect("count"), 1, "the session duplicated");
        assert_eq!(index.list().expect("list"), vec![revised]);
    }

    #[test]
    fn a_session_can_be_found_by_id() {
        let (_dir, mut index) = index();
        index
            .upsert(&[bare_session("s", datetime!(2026-09-08 12:00:00 UTC))])
            .expect("upsert");

        assert!(index
            .contains(&SessionId::derive(&provider(), "s"))
            .unwrap());
        assert!(!index
            .contains(&SessionId::derive(&provider(), "never-archived"))
            .unwrap());
    }

    #[test]
    fn an_empty_batch_writes_nothing() {
        let (_dir, mut index) = index();
        assert_eq!(index.upsert(&[]).expect("upsert"), 0);
        assert_eq!(index.count().expect("count"), 0);
    }

    #[test]
    fn clearing_empties_the_index() {
        let (_dir, mut index) = index();
        index
            .upsert(&[bare_session("s", datetime!(2026-09-08 12:00:00 UTC))])
            .expect("upsert");
        index.clear().expect("clear");
        assert_eq!(index.count().expect("count"), 0);
    }

    #[test]
    fn replacing_swaps_the_contents() {
        // What a rebuild does: the archives are the input, and whatever the
        // index held before is irrelevant.
        let (_dir, mut index) = index();
        index
            .upsert(&[bare_session("stale", datetime!(2025-01-01 00:00:00 UTC))])
            .expect("upsert");

        index
            .replace_all(&[bare_session("current", datetime!(2026-09-08 12:00:00 UTC))])
            .expect("replace");

        let ids: Vec<String> = index
            .list()
            .expect("list")
            .into_iter()
            .map(|s| s.session.provider_session_id)
            .collect();
        assert_eq!(ids, ["current"]);
    }

    #[test]
    fn a_file_that_is_not_a_database_is_replaced() {
        // The index is derived, so there is nothing in a broken file to
        // salvage. Refusing to run until the user deletes it by hand would fail
        // a command that has everything it needs to succeed.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("index.db");
        fs::write(&path, b"this is not a database").expect("write");

        let (index, opened) = Index::open(&path).expect("open");
        assert_eq!(opened.recovered, Recovered::Unreadable);
        assert!(opened.created);
        assert_eq!(index.count().expect("count"), 0);
    }

    #[test]
    fn a_schema_from_a_newer_build_is_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("index.db");

        {
            let (index, _) = Index::open(&path).expect("open");
            schema::set_version(&index.connection, SCHEMA_VERSION + 99).expect("bump");
        }

        let (index, opened) = Index::open(&path).expect("reopen");
        assert_eq!(
            opened.recovered,
            Recovered::SchemaVersion {
                found: SCHEMA_VERSION + 99
            }
        );
        assert_eq!(
            schema::version(&index.connection).expect("version"),
            SCHEMA_VERSION,
            "the rebuilt index did not get this build's schema"
        );
    }

    #[test]
    fn recovery_removes_the_sidecar_files_too() {
        // WAL leaves index.db-wal beside the database. Replacing the database
        // while keeping a write-ahead log from the old one would hand SQLite a
        // log that does not match its database.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("index.db");
        fs::write(&path, b"not a database").expect("write");
        fs::write(dir.path().join("index.db-wal"), b"stale log").expect("write");

        let (_index, opened) = Index::open(&path).expect("open");
        assert_eq!(opened.recovered, Recovered::Unreadable);
        assert!(
            !dir.path().join("index.db-wal").exists()
                || fs::read(dir.path().join("index.db-wal")).expect("read") != b"stale log",
            "the stale write-ahead log survived"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_text_is_reported_rather_than_mangled() {
        // to_string_lossy would store a path that points somewhere else. A row
        // that is missing is recoverable; one that quietly lies is not.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let (_dir, mut index) = index();
        let mut session = bare_session("s", datetime!(2026-09-08 12:00:00 UTC));
        session.session.project = Some(PathBuf::from(OsStr::from_bytes(b"/w/\xff\xfe")));

        let err = index.upsert(&[session]).expect_err("must refuse");
        assert!(matches!(err, IndexError::PathNotUtf8 { .. }), "got {err:?}");
        assert_eq!(index.count().expect("count"), 0, "a partial row landed");
    }

    #[test]
    fn a_row_whose_id_disagrees_with_its_provider_fields_is_refused() {
        // Trusting either half would attach one session's metadata to another
        // session's archive.
        let (_dir, mut index) = index();
        index
            .upsert(&[bare_session("s", datetime!(2026-09-08 12:00:00 UTC))])
            .expect("upsert");
        index
            .connection
            .execute(
                "UPDATE sessions SET provider_session_id = 'someone-else'",
                [],
            )
            .expect("tamper");

        let err = index.list().expect_err("must refuse");
        assert!(matches!(err, IndexError::Malformed { .. }), "got {err:?}");
    }

    #[test]
    fn index_holds_no_conversation_content() {
        // The boundary #37 documents. Events are the archive's job; a session
        // handed here with a transcript must not leave any of it behind.
        let (dir, mut index) = index();
        let mut indexed = bare_session("s", datetime!(2026-09-08 12:00:00 UTC));
        indexed.session.events = vec![SessionEvent::UserMessage {
            at: None,
            content: "a-secret-nobody-should-find-in-the-database".into(),
        }];
        index.upsert(&[indexed]).expect("upsert");
        drop(index);

        // Search the file itself, not the API: the point is that the bytes on
        // disk do not contain it.
        let bytes = fs::read(dir.path().join("index.db")).expect("read");
        assert!(
            !bytes
                .windows(b"a-secret-nobody-should-find-in-the-database".len())
                .any(|w| w == b"a-secret-nobody-should-find-in-the-database"),
            "conversation content reached the database"
        );
    }

    #[test]
    fn the_schema_rejects_a_value_of_the_wrong_type() {
        // What STRICT buys: a bad write fails where it happens, rather than
        // becoming a surprise in whichever command reads the row next.
        let (_dir, mut index) = index();
        index
            .upsert(&[bare_session("s", datetime!(2026-09-08 12:00:00 UTC))])
            .expect("upsert");

        let wrong = index
            .connection
            .execute("UPDATE sessions SET event_count = 'not a number'", []);
        assert!(wrong.is_err(), "STRICT is not in force");
    }
}
