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

    /// The config file exists but could not be read.
    #[error("could not read {}", .path.display())]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The config file exists but is not valid TOML, or is missing
    /// `format_version`.
    #[error("{} is not a valid Recall config", .path.display())]
    MalformedConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// The archive was written by a version of Recall this build does not
    /// understand. Guessing at an unknown layout is how an archive gets
    /// corrupted, so this is fatal.
    #[error(
        "archive at {} has format version {found}, but this build of Recall supports {supported}",
        .path.display()
    )]
    UnsupportedFormatVersion {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    /// Sessions are archived but the config that says how to read them is gone.
    #[error(
        "{} holds archived sessions but {} is missing, so their format is unknown",
        .archive.display(),
        .config.display()
    )]
    ArchivedSessionsWithoutConfig { archive: PathBuf, config: PathBuf },
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
    if config.exists() {
        // Never rewrite a config that is already there. If this build cannot
        // read it, stop rather than assume the archive matches what we expect.
        check_format_version(&config)?;
    } else if has_archived_sessions(&layout.sessions())? {
        // Sessions exist but nothing records their format. Writing a config
        // that claims the current version would be a guess, and a wrong guess
        // here misreads every archive under it.
        return Err(InitError::ArchivedSessionsWithoutConfig {
            archive: layout.root().to_path_buf(),
            config,
        });
    } else {
        // An empty archive with no config is an interrupted init. Completing it
        // is safe precisely because there is nothing to mislabel.
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

/// The subset of `config.toml` that initialization cares about.
#[derive(serde::Deserialize)]
struct Config {
    format_version: u32,
}

/// Read `config.toml` and refuse a version this build does not understand.
fn check_format_version(config: &Path) -> Result<(), InitError> {
    let text = fs::read_to_string(config).map_err(|source| InitError::ReadConfig {
        path: config.to_path_buf(),
        source,
    })?;
    let parsed: Config = toml::from_str(&text).map_err(|source| InitError::MalformedConfig {
        path: config.to_path_buf(),
        source,
    })?;
    if parsed.format_version != FORMAT_VERSION {
        return Err(InitError::UnsupportedFormatVersion {
            path: config.to_path_buf(),
            found: parsed.format_version,
            supported: FORMAT_VERSION,
        });
    }
    Ok(())
}

/// Whether any session has been archived under `sessions`.
///
/// Stops at the first file found; this only ever answers yes or no.
fn has_archived_sessions(sessions: &Path) -> Result<bool, InitError> {
    let mut stack = vec![sessions.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(InitError::ReadDirectory { path: dir, source }),
        };
        for entry in entries {
            let entry = entry.map_err(|source| InitError::ReadDirectory {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                return Ok(true);
            }
        }
    }
    Ok(false)
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

    /// Put a session-shaped file in the archive, so a test can prove it survives.
    fn archive_a_session(root: &Path, name: &str) -> PathBuf {
        let day = Layout::for_project(root).sessions().join("2026/09/08");
        fs::create_dir_all(&day).expect("create day directory");
        let path = day.join(name);
        fs::write(&path, b"pretend this is a compressed session").expect("write session");
        path
    }

    #[test]
    fn rerunning_never_touches_archived_sessions() {
        // The archive may be the only copy of a conversation. This is the test
        // that matters most in this file.
        let p = project();
        init(p.path()).expect("init");
        let session = archive_a_session(p.path(), "abc.zst");

        for _ in 0..3 {
            init(p.path()).expect("re-init");
        }

        assert_eq!(
            fs::read(&session).expect("read"),
            b"pretend this is a compressed session"
        );
    }

    #[test]
    fn completes_an_interrupted_init() {
        // .recall/ exists but the run died before creating everything.
        let p = project();
        let l = Layout::for_project(p.path());
        fs::create_dir(l.root()).expect("create root");

        let outcome = init(p.path()).expect("init should finish the job");

        assert!(l.sessions().is_dir());
        assert!(l.tmp().is_dir());
        assert!(l.config().is_file());
        assert!(outcome.created_anything());
    }

    #[test]
    fn refuses_an_archive_whose_format_it_does_not_know() {
        let p = project();
        init(p.path()).expect("init");
        let config = Layout::for_project(p.path()).config();
        fs::write(&config, "format_version = 99\n").expect("write");

        let err = init(p.path()).expect_err("an unknown format version must be fatal");
        assert!(
            matches!(err, InitError::UnsupportedFormatVersion { found: 99, .. }),
            "{err:?}"
        );

        // And the config we could not understand is left exactly as it was.
        assert_eq!(
            fs::read_to_string(&config).expect("read"),
            "format_version = 99\n"
        );
    }

    #[test]
    fn refuses_a_malformed_config() {
        let p = project();
        init(p.path()).expect("init");
        fs::write(
            Layout::for_project(p.path()).config(),
            "this is not toml at all {{{",
        )
        .expect("write");

        let err = init(p.path()).expect_err("a malformed config must be fatal");
        assert!(matches!(err, InitError::MalformedConfig { .. }), "{err:?}");
    }

    #[test]
    fn refuses_to_label_sessions_whose_format_is_unknown() {
        // Sessions archived, config gone. Writing a current-version config here
        // would claim something we cannot know, and every archive under it
        // would then be read wrongly.
        let p = project();
        init(p.path()).expect("init");
        archive_a_session(p.path(), "abc.zst");
        fs::remove_file(Layout::for_project(p.path()).config()).expect("remove config");

        let err = init(p.path()).expect_err("must not guess the format");
        assert!(
            matches!(err, InitError::ArchivedSessionsWithoutConfig { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn restores_a_config_lost_from_an_empty_archive() {
        // Nothing archived yet, so there is nothing to mislabel.
        let p = project();
        init(p.path()).expect("init");
        let config = Layout::for_project(p.path()).config();
        fs::remove_file(&config).expect("remove config");

        init(p.path()).expect("init should complete the archive");
        assert!(config.is_file());
    }

    #[test]
    fn refuses_when_an_owned_path_is_the_wrong_kind() {
        let p = project();
        let l = Layout::for_project(p.path());
        fs::create_dir(l.root()).expect("create root");
        fs::write(l.sessions(), b"a file where a directory belongs").expect("write");

        let err = init(p.path()).expect_err("sessions must be a directory");
        assert!(matches!(err, InitError::NotADirectory { .. }), "{err:?}");
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
