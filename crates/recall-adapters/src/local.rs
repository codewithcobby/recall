//! Small filesystem helpers every adapter needs.
//!
//! Each of these encodes a rule from `.github/CONTRIBUTING.md` that would
//! otherwise be re-decided per adapter — where a home directory comes from,
//! and what an absent provider directory means. Sharing them keeps the answer
//! the same for every provider.
//!
//! Nothing here is provider-specific, and nothing here writes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use recall_core::AdapterError;
use time::OffsetDateTime;

/// The user's home directory.
///
/// Resolved from the environment rather than by walking anywhere: Recall never
/// searches for a provider's files speculatively.
pub fn home_directory() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .or_else(|| {
                let drive = std::env::var_os("HOMEDRIVE")?;
                let path = std::env::var_os("HOMEPATH")?;
                let mut home = PathBuf::from(drive);
                home.push(path);
                Some(home)
            })
    }
}

/// List a directory, treating "not there" as empty rather than as a failure.
///
/// A missing directory means the agent is not installed, or has never run.
/// Both are ordinary conditions during a sync. Unreadable is different from
/// absent, and is worth saying so.
///
/// The returned paths are sorted, so two runs report the same thing.
pub fn read_directory(dir: &Path) -> Result<Vec<PathBuf>, AdapterError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AdapterError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| AdapterError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

/// When a file was last written, if the filesystem will say.
pub fn modified_at(path: &Path) -> Option<OffsetDateTime> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(OffsetDateTime::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_directory_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(read_directory(&dir.path().join("nope"))
            .expect("absent is not an error")
            .is_empty());
    }

    #[test]
    fn entries_come_back_in_a_stable_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["zzz", "aaa", "mmm"] {
            fs::write(dir.path().join(name), b"x").expect("write");
        }
        let once = read_directory(dir.path()).expect("read");
        let again = read_directory(dir.path()).expect("read");
        assert_eq!(once, again, "two runs disagreed about ordering");
        let names: Vec<_> = once
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert_eq!(names, ["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn a_file_reports_when_it_was_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("f");
        fs::write(&path, b"x").expect("write");
        assert!(modified_at(&path).is_some());
        assert!(modified_at(&dir.path().join("absent")).is_none());
    }
}
