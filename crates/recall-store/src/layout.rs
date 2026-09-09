//! Where things live inside `.recall/`.
//!
//! Every path Recall touches is derived here, from the project root. Nothing is
//! ever built from a value read out of a provider session file — see
//! `.github/SECURITY.md`.
//!
//! The layout itself is specified in `docs/archive-layout.md`.

use std::path::{Path, PathBuf};

use recall_core::SessionId;

/// The archive directory, relative to the project root.
pub const ARCHIVE_DIR: &str = ".recall";

/// Format version and project settings.
pub const CONFIG_FILE: &str = "config.toml";

/// Archived sessions, partitioned by UTC start date.
pub const SESSIONS_DIR: &str = "sessions";

/// Staging for atomic writes. Must sit inside the archive so that renaming a
/// staged file into `sessions/` stays within one filesystem.
pub const TMP_DIR: &str = "tmp";

/// The derived metadata index.
pub const INDEX_FILE: &str = "index.db";

/// Extension for a session archive.
///
/// The encoding is named by the extension, so a reader knows how to decode a
/// file before opening it. This is what archives are written as today:
/// Zstandard-compressed JSON Lines.
pub const SESSION_EXTENSION: &str = "zst";

/// Extension for an uncompressed session archive.
///
/// Written before compression landed in #16. Still read, so an archive created
/// by an earlier build stays readable without a migration — which is the whole
/// point of naming the encoding in the extension.
pub const UNCOMPRESSED_SESSION_EXTENSION: &str = "jsonl";

/// Zstandard level used for new archives.
///
/// Sessions are written once and read rarely, so this favours ratio over write
/// speed. Measured over a 620 KB synthetic transcript of agent work — prose,
/// code in tool results, command output, JSON arguments:
///
/// | level | bytes | ratio | compress | decompress |
/// |-------|-------|-------|----------|------------|
/// | 3 (default) | 73,635 | 8.4x | 2.0 ms | 674 µs |
/// | 9 | 68,128 | 9.1x | 10.6 ms | 377 µs |
/// | **12** | **62,950** | **9.9x** | **10.4 ms** | **223 µs** |
/// | 15 | 59,736 | 10.4x | 26.6 ms | 172 µs |
/// | 19 | 56,092 | 11.1x | 128.9 ms | 188 µs |
///
/// 12 is where the curve bends: 14% smaller than the default for about 10 ms on
/// a large session, where 19 costs 64 times the default's write time for a
/// further 10%. Higher levels also *decompress* faster here, since there is
/// less to read back.
///
/// Ratios on real transcripts will differ; the shape of the curve is the part
/// that generalises.
pub const COMPRESSION_LEVEL: i32 = 12;

/// The archive layout this build understands.
///
/// Bumped when the meaning of an existing key changes, a file moves, or the
/// session encoding changes. Adding an optional key is not a bump.
pub const FORMAT_VERSION: u32 = 1;

/// Resolves the paths Recall owns for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The layout for the project rooted at `project_root`.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self {
            root: project_root.as_ref().join(ARCHIVE_DIR),
        }
    }

    /// The `.recall/` directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.recall/config.toml`
    pub fn config(&self) -> PathBuf {
        self.root.join(CONFIG_FILE)
    }

    /// `.recall/sessions`
    pub fn sessions(&self) -> PathBuf {
        self.root.join(SESSIONS_DIR)
    }

    /// `.recall/tmp`
    pub fn tmp(&self) -> PathBuf {
        self.root.join(TMP_DIR)
    }

    /// `.recall/index.db`
    pub fn index(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    /// Where a session archived on this UTC date belongs.
    ///
    /// `date` is `(year, month, day)` in UTC, as
    /// [`Session::archive_date`](recall_core::Session::archive_date) returns.
    pub fn session_dir(&self, date: (i32, u8, u8)) -> PathBuf {
        let (year, month, day) = date;
        self.sessions()
            .join(format!("{year:04}"))
            .join(format!("{month:02}"))
            .join(format!("{day:02}"))
    }

    /// The full path an uncompressed session is written to.
    pub fn session_path(&self, date: (i32, u8, u8), id: &SessionId) -> PathBuf {
        self.session_dir(date)
            .join(format!("{}.{}", id, SESSION_EXTENSION))
    }

    /// The directories `recall init` creates, outermost first.
    pub fn directories(&self) -> [PathBuf; 3] {
        [self.root.clone(), self.sessions(), self.tmp()]
    }

    /// Whether `name` is an entry Recall owns.
    ///
    /// Anything else inside `.recall/` belongs to whoever put it there and is
    /// never read, moved, or deleted.
    pub fn owns(name: &str) -> bool {
        matches!(name, CONFIG_FILE | SESSIONS_DIR | TMP_DIR | INDEX_FILE)
            // SQLite's write-ahead log and shared-memory files sit beside the
            // database and belong to it.
            || name.starts_with(INDEX_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_hang_off_the_project_root() {
        let l = Layout::for_project("/p");
        assert_eq!(l.root(), Path::new("/p/.recall"));
        assert_eq!(l.config(), Path::new("/p/.recall/config.toml"));
        assert_eq!(l.sessions(), Path::new("/p/.recall/sessions"));
        assert_eq!(l.tmp(), Path::new("/p/.recall/tmp"));
        assert_eq!(l.index(), Path::new("/p/.recall/index.db"));
    }

    #[test]
    fn staging_shares_a_filesystem_with_sessions() {
        // If this ever stops holding, renaming a staged session into place
        // silently stops being atomic.
        let l = Layout::for_project("/p");
        assert_eq!(l.tmp().parent(), l.sessions().parent());
    }

    #[test]
    fn recognises_what_it_owns() {
        assert!(Layout::owns("config.toml"));
        assert!(Layout::owns("sessions"));
        assert!(Layout::owns("tmp"));
        assert!(Layout::owns("index.db"));
        assert!(Layout::owns("index.db-wal"));
        assert!(Layout::owns("index.db-shm"));
    }

    #[test]
    fn leaves_everything_else_alone() {
        for name in ["notes.md", ".DS_Store", "sessions.bak", "config.toml.old"] {
            assert!(!Layout::owns(name), "{name} must not be treated as ours");
        }
    }
}
