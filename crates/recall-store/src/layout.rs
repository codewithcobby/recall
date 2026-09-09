//! Where things live inside `.recall/`.
//!
//! Every path Recall touches is derived here, from the project root. Nothing is
//! ever built from a value read out of a provider session file — see
//! `.github/SECURITY.md`.
//!
//! The layout itself is specified in `docs/archive-layout.md`.

use std::path::{Path, PathBuf};

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
