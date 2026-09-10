//! The database's shape, and the version that identifies it.
//!
//! The index is derived data: every column here can be recomputed from the
//! archives. That is what makes a corrupt or deleted `index.db` an
//! inconvenience rather than a loss, and it is the property #37 exists to
//! protect.

use rusqlite::Connection;

/// The schema this build understands.
///
/// Bumped when a column changes meaning or disappears. Because the index is
/// derived, an unrecognised version never has to be migrated in place: it can
/// always be discarded and rebuilt from the archives.
pub const SCHEMA_VERSION: u32 = 1;

/// The initial schema.
///
/// `STRICT` is deliberate. Without it SQLite accepts a string in an integer
/// column and hands it back later as a surprise; the index is written by sync
/// and read by the listing, so type confusion would surface far from its cause.
///
/// Timestamps are text in a fixed-width canonical UTC form (see the
/// `timestamp` module). Fixed width is what makes lexicographic ordering in SQL
/// identical to chronological ordering, which is how the listing sorts without
/// reading every row.
pub const SCHEMA: &str = r#"
CREATE TABLE sessions (
    id                  TEXT PRIMARY KEY NOT NULL,
    provider            TEXT NOT NULL,
    provider_session_id TEXT NOT NULL,
    model               TEXT,
    started_at          TEXT NOT NULL,
    ended_at            TEXT,
    project             TEXT,
    repository          TEXT,
    branch              TEXT,
    commit_at_start     TEXT,
    commit_at_end       TEXT,
    event_count         INTEGER,
    archive_path        TEXT NOT NULL
) STRICT;

-- The listing is newest-first, and it is the only query that exists today.
-- Ordering by a fixed-width timestamp lets SQLite walk this index rather than
-- sort the table.
CREATE INDEX sessions_by_start ON sessions (started_at DESC);

-- A session's id is derived from exactly this pair, so a duplicate here would
-- mean two ids for one session. Asserted in the schema rather than trusted,
-- because more than one code path writes rows.
CREATE UNIQUE INDEX sessions_by_provider_id ON sessions (provider, provider_session_id);

-- What #38's search is built on: which sessions touched this project, and
-- which ran on this branch.
CREATE INDEX sessions_by_project ON sessions (project);
CREATE INDEX sessions_by_branch  ON sessions (branch);
"#;

/// Read the schema version from the file header.
///
/// `user_version` rather than a table of our own: it costs no row, it exists
/// before any schema does, and it is readable from a fresh file — which is
/// exactly the moment the version needs deciding.
pub fn version(connection: &Connection) -> rusqlite::Result<u32> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Record the schema version.
pub fn set_version(connection: &Connection, version: u32) -> rusqlite::Result<()> {
    // Not a bound parameter: SQLite does not accept one in a PRAGMA. The value
    // is a u32 this crate chose, never anything read from outside.
    connection.execute_batch(&format!("PRAGMA user_version = {version}"))
}
