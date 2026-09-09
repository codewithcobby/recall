//! Creating `.recall/` for a project.
//!
//! Initialization is non-destructive. It creates what is missing and never
//! rewrites, moves, or deletes anything that is already there — including files
//! Recall does not recognise, which belong to whoever put them in `.recall/`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::layout::{Layout, FORMAT_VERSION};

/// Why initialization could not complete.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// Something is in the way that is not a directory.
    #[error("{} exists but is not a directory", .path.display())]
    NotADirectory { path: PathBuf },

    /// A directory could not be created.
    #[error("could not create {}", .path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A file could not be written.
    #[error("could not write {}", .path.display())]
    WriteFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// `.recall/` could not be read to work out what is already there.
    #[error("could not read {}", .path.display())]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What initialization did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// The `.recall/` directory.
    pub root: PathBuf,
    /// Paths created by this run, in the order they were created. Empty when
    /// everything already existed.
    pub created: Vec<PathBuf>,
    /// Entries inside `.recall/` that Recall does not own. Reported so the user
    /// knows they were seen; never touched.
    pub unrecognized: Vec<String>,
}

impl InitOutcome {
    /// Whether this run created anything at all.
    pub fn created_anything(&self) -> bool {
        !self.created.is_empty()
    }
}

/// Create `.recall/` for the project rooted at `project_root`.
///
/// Safe to call on a project that is already initialized: existing directories
/// and an existing config file are left exactly as they are.
pub fn init(project_root: impl AsRef<Path>) -> Result<InitOutcome, InitError> {
    let layout = Layout::for_project(project_root);
    let mut created = Vec::new();

    for dir in layout.directories() {
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(InitError::NotADirectory { path: dir }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&dir).map_err(|source| InitError::CreateDirectory {
                    path: dir.clone(),
                    source,
                })?;
                restrict_to_owner(&dir, Permissions::Directory)?;
                created.push(dir);
            }
            Err(source) => {
                return Err(InitError::ReadDirectory { path: dir, source });
            }
        }
    }

    let config = layout.config();
    if !config.exists() {
        fs::write(&config, config_contents()).map_err(|source| InitError::WriteFile {
            path: config.clone(),
            source,
        })?;
        restrict_to_owner(&config, Permissions::File)?;
        created.push(config);
    }

    Ok(InitOutcome {
        unrecognized: unrecognized_entries(layout.root())?,
        root: layout.root().to_path_buf(),
        created,
    })
}

/// The initial `config.toml`.
fn config_contents() -> String {
    format!(
        "# Recall archive configuration. See docs/archive-layout.md.\n\
         #\n\
         # format_version is read before anything else in .recall/ is touched.\n\
         # A version Recall does not understand is a hard error, never a\n\
         # best-effort read.\n\
         format_version = {FORMAT_VERSION}\n"
    )
}

/// Entries inside `.recall/` that Recall does not own, sorted for stable output.
fn unrecognized_entries(root: &Path) -> Result<Vec<String>, InitError> {
    let mut found = Vec::new();
    let entries = fs::read_dir(root).map_err(|source| InitError::ReadDirectory {
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| InitError::ReadDirectory {
            path: root.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !Layout::owns(&name) {
            found.push(name);
        }
    }
    found.sort();
    Ok(found)
}

enum Permissions {
    Directory,
    File,
}

/// Restrict a path to its owner.
///
/// Archives hold conversation content, which routinely includes proprietary
/// source and occasionally a credential pasted into a prompt, so other users on
/// the machine must not be able to read them.
#[cfg(unix)]
fn restrict_to_owner(path: &Path, kind: Permissions) -> Result<(), InitError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = match kind {
        Permissions::Directory => 0o700,
        Permissions::File => 0o600,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        InitError::WriteFile {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Windows inherits the parent ACL. Matching the Unix guarantee there is #62.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path, _kind: Permissions) -> Result<(), InitError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    /// Every test gets its own project directory. A test that touched the real
    /// home directory or a real provider location would be rejected in review.
    fn project() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn creates_the_documented_layout() {
        let p = project();
        let outcome = init(p.path()).expect("init");

        let l = Layout::for_project(p.path());
        assert!(l.root().is_dir());
        assert!(l.sessions().is_dir());
        assert!(l.tmp().is_dir());
        assert!(l.config().is_file());
        assert!(
            !l.index().exists(),
            "the index is created when first needed"
        );
        assert!(outcome.created_anything());
    }

    #[test]
    fn config_records_the_format_version() {
        let p = project();
        init(p.path()).expect("init");

        let config = fs::read_to_string(Layout::for_project(p.path()).config()).expect("read");
        assert!(
            config.contains(&format!("format_version = {FORMAT_VERSION}")),
            "config.toml did not record the format version: {config}"
        );
    }

    #[test]
    fn a_second_run_creates_nothing() {
        let p = project();
        init(p.path()).expect("first init");
        let second = init(p.path()).expect("second init");

        assert!(!second.created_anything(), "created {:?}", second.created);
    }

    #[test]
    fn refuses_when_something_is_in_the_way() {
        let p = project();
        fs::write(p.path().join(".recall"), b"not a directory").expect("write");

        let err = init(p.path()).expect_err("a file named .recall must not be clobbered");
        assert!(matches!(err, InitError::NotADirectory { .. }), "{err:?}");

        // The file is still exactly as it was.
        let still_there = fs::read(p.path().join(".recall")).expect("read");
        assert_eq!(still_there, b"not a directory");
    }

    #[test]
    fn reports_entries_it_does_not_own_without_touching_them() {
        let p = project();
        init(p.path()).expect("init");

        let stray = Layout::for_project(p.path()).root().join("notes.md");
        fs::write(&stray, b"mine, not Recall's").expect("write");

        let outcome = init(p.path()).expect("re-init");
        assert_eq!(outcome.unrecognized, vec!["notes.md".to_string()]);
        assert_eq!(fs::read(&stray).expect("read"), b"mine, not Recall's");
    }

    #[cfg(unix)]
    #[test]
    fn archive_is_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let p = project();
        init(p.path()).expect("init");
        let l = Layout::for_project(p.path());

        for dir in l.directories() {
            let mode = fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}, expected 700", dir.display());
        }

        let mode = fs::metadata(l.config())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "config.toml is {mode:o}, expected 600");
    }
}
