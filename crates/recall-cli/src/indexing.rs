//! Where the archive and the index meet.
//!
//! `recall-index` never reads an archive and `recall-store` never opens a
//! database — neither depends on the other, and the dependency direction in
//! `.github/CONTRIBUTING.md` says they must not. This module is the composition
//! the binary is allowed to do, and it is the only place that knows both.
//!
//! The rule everything here follows: **the archive wins.** A session that is
//! archived is preserved, whatever the index does afterwards. An index that
//! falls behind is a performance problem that the next sync repairs; an archive
//! that is not written is a conversation that is gone.

use std::path::Path;

use recall_core::{SessionHeader, SessionId};
use recall_index::{Index, IndexError, IndexedSession, Recovered};
use recall_store::Archive;

/// Rows are written in batches of this size.
///
/// A transaction per session turns a first sync of a few hundred sessions into
/// a few hundred fsyncs. Batching them is the difference between a sync that
/// feels instant and one that does not — and the batch is bounded so that a
/// very large archive does not have to be held in memory all at once.
const BATCH: usize = 500;

/// Open the index for a project, creating it if it is missing.
pub fn open(archive: &Archive) -> Result<(Index, Recovered), IndexError> {
    let (index, opened) = Index::open(archive.layout().index())?;
    Ok((index, opened.recovered))
}

/// Build the row for a session whose archive is at `path`.
///
/// The stored path is relative to `.recall/`, so moving or copying a project
/// does not invalidate every row.
pub fn row(header: SessionHeader, archive: &Archive, path: &Path) -> IndexedSession {
    let relative = path
        .strip_prefix(archive.layout().root())
        .unwrap_or(path)
        .to_path_buf();
    IndexedSession::new(header, relative)
}

/// Collects rows and writes them in bounded batches.
///
/// Errors are held rather than returned: the caller is in the middle of
/// archiving, and an index that will not accept a row is not a reason to stop
/// preserving conversations.
#[derive(Default)]
pub struct Batch {
    pending: Vec<IndexedSession>,
    /// How many rows reached the database.
    pub written: usize,
    /// The first thing that went wrong, if anything did.
    pub problem: Option<String>,
}

impl Batch {
    /// Queue a row, flushing if the batch is full.
    pub fn add(&mut self, index: &mut Index, row: IndexedSession) {
        self.pending.push(row);
        if self.pending.len() >= BATCH {
            self.flush(index);
        }
    }

    /// Write whatever is queued.
    pub fn flush(&mut self, index: &mut Index) {
        if self.pending.is_empty() {
            return;
        }
        match index.upsert(&self.pending) {
            Ok(n) => self.written += n,
            // Kept, not returned. The sessions in this batch are already in the
            // archive; the next sync will find them missing from the index and
            // put them back.
            Err(e) => self.remember(&e),
        }
        self.pending.clear();
    }

    fn remember(&mut self, error: &IndexError) {
        if self.problem.is_none() {
            self.problem = Some(error.to_string());
        }
    }
}

/// Whether a session still needs indexing, erring towards indexing it.
///
/// A failed lookup answers "yes": re-indexing a session that was already there
/// costs one upsert, while skipping one that was missing leaves it invisible to
/// the listing until something else notices.
pub fn needs_indexing(index: &Index, id: &SessionId) -> bool {
    !index.contains(id).unwrap_or(false)
}

/// Read just the metadata of an already-archived session.
///
/// Used to reconcile a session the archive has and the index does not. Reads
/// the archive's first line rather than the whole transcript.
pub fn header_of(archive: &Archive, id: &SessionId) -> Option<SessionHeader> {
    archive.stream(id).ok().map(|s| s.header().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use recall_core::{Provider, Session};
    use std::path::PathBuf;

    fn header(id: &str) -> SessionHeader {
        SessionHeader {
            session: Session::new(
                Provider::new("claude-code").expect("provider"),
                id,
                time::OffsetDateTime::now_utc(),
            ),
            event_count: Some(1),
        }
    }

    #[test]
    fn the_stored_path_is_relative_to_the_archive() {
        // Absolute paths would make every row wrong the moment a project moved.
        let project = tempfile::tempdir().expect("temp dir");
        let archive = Archive::open(project.path());
        let absolute = archive.layout().root().join("sessions/2026/09/08/abc.zst");

        let row = row(header("s"), &archive, &absolute);
        assert_eq!(
            row.archive_path,
            PathBuf::from("sessions/2026/09/08/abc.zst")
        );
    }

    #[test]
    fn a_path_outside_the_archive_is_kept_as_it_is() {
        // Should not happen, but truncating it to a misleading relative path
        // would be worse than storing something obviously absolute.
        let project = tempfile::tempdir().expect("temp dir");
        let archive = Archive::open(project.path());
        let elsewhere = PathBuf::from("/somewhere/else/abc.zst");

        let row = row(header("s"), &archive, &elsewhere);
        assert_eq!(row.archive_path, elsewhere);
    }

    #[test]
    fn a_batch_writes_nothing_until_it_is_flushed() {
        let project = tempfile::tempdir().expect("temp dir");
        recall_store::init(project.path()).expect("init");
        let archive = Archive::open(project.path());
        let (mut index, _) = open(&archive).expect("index");

        let mut batch = Batch::default();
        batch.add(&mut index, row(header("s"), &archive, Path::new("a.zst")));
        assert_eq!(index.count().expect("count"), 0);

        batch.flush(&mut index);
        assert_eq!(index.count().expect("count"), 1);
        assert_eq!(batch.written, 1);
        assert_eq!(batch.problem, None);
    }

    #[test]
    fn a_full_batch_flushes_itself() {
        let project = tempfile::tempdir().expect("temp dir");
        recall_store::init(project.path()).expect("init");
        let archive = Archive::open(project.path());
        let (mut index, _) = open(&archive).expect("index");

        let mut batch = Batch::default();
        for n in 0..BATCH {
            batch.add(
                &mut index,
                row(header(&format!("s{n}")), &archive, Path::new("a.zst")),
            );
        }
        // Reached the size limit, so it wrote without being asked.
        assert_eq!(index.count().expect("count"), BATCH);
    }

    #[test]
    fn flushing_an_empty_batch_does_nothing() {
        let project = tempfile::tempdir().expect("temp dir");
        recall_store::init(project.path()).expect("init");
        let archive = Archive::open(project.path());
        let (mut index, _) = open(&archive).expect("index");

        let mut batch = Batch::default();
        batch.flush(&mut index);
        assert_eq!(batch.written, 0);
        assert_eq!(batch.problem, None);
    }

    #[test]
    fn an_unindexed_session_is_reported_as_needing_indexing() {
        let project = tempfile::tempdir().expect("temp dir");
        recall_store::init(project.path()).expect("init");
        let archive = Archive::open(project.path());
        let (mut index, _) = open(&archive).expect("index");

        let id = SessionId::derive(&Provider::new("claude-code").unwrap(), "s");
        assert!(needs_indexing(&index, &id));

        index
            .upsert(&[row(header("s"), &archive, Path::new("a.zst"))])
            .expect("upsert");
        assert!(!needs_indexing(&index, &id));
    }
}
