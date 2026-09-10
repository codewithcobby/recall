//! Gemini CLI.
//!
//! Google's open-source terminal agent, installed as `@google/gemini-cli`.
//! Sessions live under a per-project directory:
//!
//! ```text
//! ~/.gemini/tmp/<project-name>/chats/session-<timestamp>-<id>.jsonl
//! ~/.gemini/tmp/<project-name>/.project_root      the absolute project path
//! ```
//!
//! On Windows the same tree sits under `%USERPROFILE%\.gemini`.
//!
//! **`~/.gemini` is not the Gemini CLI's alone.** Antigravity — a different
//! Google product, invoked as `agy` — stores its own conversations in
//! `~/.gemini/antigravity-cli/` and `antigravity-ide/`, as SQLite with
//! protobuf payloads. Those are not Gemini CLI sessions and must never be read
//! as though they were, which is why discovery scopes itself to `tmp/` rather
//! than walking the home directory.
//!
//! The project directory is named for the project's last path component, which
//! collides across projects of the same name and cannot be reversed. It is
//! never trusted: `.project_root` beside it records the absolute path exactly.
//!
//! The id in a filename is only the first 8 characters of the session's real
//! id, so the `sessionId` on the file's own header record is what identifies a
//! session.
//!
//! Verified against Gemini CLI 0.59.0. The format is documented in
//! `docs/providers/gemini-cli.md`.
//!
//! Parsing and normalization are #46, fixtures #47.

use std::fs;
use std::io;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use recall_core::{Adapter, AdapterError, DiscoveredSession, Provider, Session};

use crate::local::{home_directory, modified_at, read_directory};

/// The provider name Gemini CLI sessions are recorded under.
///
/// Named for the CLI rather than the model: `gemini` is a family of models that
/// several tools call, and a session belongs to the agent that produced it.
pub const PROVIDER: &str = "gemini-cli";

/// Directory under the user's home where the Gemini CLI keeps its state.
///
/// Shared with Antigravity, which is why discovery never reads it directly.
pub const HOME_SUBDIRECTORY: &str = ".gemini";

/// Directory under [`HOME_SUBDIRECTORY`] holding the per-project directories.
///
/// Named `tmp` by the Gemini CLI, which undersells it: the transcripts under
/// here are the only record of a conversation.
pub const PROJECTS_SUBDIRECTORY: &str = "tmp";

/// Directory inside a project's directory holding its session files.
pub const CHATS_SUBDIRECTORY: &str = "chats";

/// File beside a project's sessions recording the absolute project path.
pub const PROJECT_ROOT_FILE: &str = ".project_root";

/// Extension of a Gemini CLI session file.
pub const SESSION_EXTENSION: &str = "jsonl";

/// Filename prefix the Gemini CLI gives a session.
pub const SESSION_PREFIX: &str = "session-";

/// Reads the Gemini CLI's session history.
#[derive(Debug, Clone)]
pub struct Gemini {
    provider: Provider,
    /// Where to look. Injectable so tests never touch a real home directory.
    root: Option<PathBuf>,
}

impl Gemini {
    /// An adapter reading the current user's Gemini CLI sessions.
    pub fn new() -> Self {
        Self {
            provider: Provider::new(PROVIDER).expect("the provider name is a valid one"),
            root: None,
        }
    }

    /// An adapter reading a specific `.gemini` directory.
    ///
    /// Tests use this. A test that read the real home directory would depend on
    /// whoever ran it, and would be reading someone's actual transcripts.
    pub fn rooted_at(gemini_home: impl Into<PathBuf>) -> Self {
        Self {
            provider: Provider::new(PROVIDER).expect("the provider name is a valid one"),
            root: Some(gemini_home.into()),
        }
    }

    /// The `.gemini` directory this adapter reads, if it can be determined.
    pub fn gemini_home(&self) -> Option<PathBuf> {
        match &self.root {
            Some(root) => Some(root.clone()),
            None => home_directory().map(|home| home.join(HOME_SUBDIRECTORY)),
        }
    }

    /// The directory holding per-project session directories.
    ///
    /// Scoped deliberately: `~/.gemini` also holds another product's
    /// conversations, and only what is under here belongs to the Gemini CLI.
    pub fn projects_directory(&self) -> Option<PathBuf> {
        self.gemini_home()
            .map(|home| home.join(PROJECTS_SUBDIRECTORY))
    }
}

impl Default for Gemini {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for Gemini {
    fn provider(&self) -> &Provider {
        &self.provider
    }

    fn search_roots(&self) -> Vec<PathBuf> {
        self.projects_directory().into_iter().collect()
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>, AdapterError> {
        let Some(projects) = self.projects_directory() else {
            // No home directory to resolve. Not an error: there is simply
            // nothing to read.
            return Ok(Vec::new());
        };

        let mut found = Vec::new();
        for project_dir in read_directory(&projects)? {
            if !project_dir.is_dir() {
                // `tmp/` holds stray files too. They are not projects.
                continue;
            }

            // The exact project path, recorded beside the sessions. The
            // directory name is only the project's last path component, so it
            // collides between two projects called `api` and cannot be
            // reversed into a path.
            let project = recorded_project(&project_dir);

            for entry in read_directory(&project_dir.join(CHATS_SUBDIRECTORY))? {
                if !is_session_file(&entry) {
                    continue;
                }
                if !entry.is_file() {
                    continue;
                }

                // The id inside the file is the whole one. The filename carries
                // only its first 8 characters, which is not enough to identify
                // a session and must never be archived as though it were.
                let Some(id) = session_id(&entry) else {
                    continue;
                };

                found.push(DiscoveredSession {
                    provider_session_id: id,
                    modified: modified_at(&entry),
                    project: project.clone(),
                    // One file per session; the Gemini CLI keeps no sibling
                    // transcripts.
                    additional_paths: Vec::new(),
                    path: entry,
                });
            }
        }

        // Stable order, so two runs report the same thing.
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    fn load(&self, discovered: &DiscoveredSession) -> Result<Session, AdapterError> {
        // Parsing is #46. Until then a session can be named but not read, and
        // saying so plainly beats handing back one invented from a format this
        // adapter has not yet confirmed.
        Err(AdapterError::Empty {
            provider: self.provider.clone(),
            provider_session_id: discovered.provider_session_id.clone(),
        })
    }
}

/// Whether a file is one of the Gemini CLI's session files.
fn is_session_file(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some(SESSION_EXTENSION) {
        return false;
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(SESSION_PREFIX))
}

/// The absolute project path, as `.project_root` records it.
///
/// The directory name cannot supply this. The Gemini CLI names a project
/// directory after the last component of its path, so `/w/api` and
/// `/other/api` both become `api` — indistinguishable, and neither reversible
/// into a path. `.project_root` holds the real thing.
fn recorded_project(project_dir: &Path) -> Option<PathBuf> {
    let recorded = fs::read_to_string(project_dir.join(PROJECT_ROOT_FILE)).ok()?;
    let trimmed = recorded.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// How many lines to read looking for the session header.
///
/// The Gemini CLI writes it first, so in practice this reads one line.
const HEADER_SEARCH_LINES: usize = 8;

/// The session's own identifier, from the header record inside the file.
///
/// Deliberately not taken from the filename. A file named
/// `session-2026-09-10T11-25-be9de464.jsonl` carries only the first 8
/// characters of `be9de464-9658-4c1b-a810-664ecc8b06e8`. Archiving that prefix
/// as the session's id would make two sessions collide on their first eight
/// characters and would not trace back to anything the provider knows.
///
/// A file whose header cannot be read is skipped rather than archived under a
/// guessed id — unlike Codex, there is no full id in the name to fall back on.
fn session_id(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;

    for line in io::BufReader::new(file).lines().take(HEADER_SEARCH_LINES) {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value.get("sessionId").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_is_named_consistently() {
        assert_eq!(Gemini::new().provider().as_str(), "gemini-cli");
    }

    #[test]
    fn no_two_providers_share_a_session_identity() {
        // The provider is part of a session's derived id, so three agents that
        // happened to use the same uuid still archive as three sessions.
        use recall_core::SessionId;
        let gemini = Gemini::new();
        let codex = crate::Codex::new();
        let claude = crate::ClaudeCode::new();

        let ids = [
            SessionId::derive(gemini.provider(), "shared-id"),
            SessionId::derive(codex.provider(), "shared-id"),
            SessionId::derive(claude.provider(), "shared-id"),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b, "two providers derived the same session id");
            }
        }
    }

    #[test]
    fn a_rooted_adapter_looks_only_where_it_was_told() {
        let adapter = Gemini::rooted_at("/somewhere/.gemini");
        assert_eq!(
            adapter.gemini_home(),
            Some(PathBuf::from("/somewhere/.gemini"))
        );
        // Scoped to `tmp/`, not the home directory: another product's
        // conversations live alongside it.
        assert_eq!(
            adapter.projects_directory(),
            Some(PathBuf::from("/somewhere/.gemini/tmp"))
        );
        assert_eq!(
            adapter.search_roots(),
            vec![PathBuf::from("/somewhere/.gemini/tmp")]
        );
    }

    #[test]
    fn the_search_root_is_reported_so_a_caller_can_say_what_was_looked_at() {
        // `recall sync` prints these when it finds nothing, so a user can tell
        // "not installed" from "looked in the wrong place".
        assert!(!Gemini::rooted_at("/somewhere/.gemini")
            .search_roots()
            .is_empty());
    }

    #[test]
    fn another_products_conversations_are_not_reported_as_sessions() {
        // `~/.gemini` is shared with Antigravity, whose conversations live in
        // `antigravity-cli/conversations/*.db`. They are a different product's
        // sessions in a different format, and reading them as Gemini CLI
        // history would archive someone else's transcripts under the wrong
        // provider.
        let home = tempfile::tempdir().expect("temp dir");
        let conversations = home.path().join("antigravity-cli").join("conversations");
        std::fs::create_dir_all(&conversations).expect("create antigravity directory");
        std::fs::write(
            conversations.join("182d4d8f-fd31-4c01-8f18-49a52c5dae60.db"),
            b"SQLite format 3\0",
        )
        .expect("write");

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert!(
            found.is_empty(),
            "Antigravity conversations were reported as Gemini CLI sessions: {found:?}"
        );
    }

    /// A fake `.gemini` home. No test touches the real one.
    fn gemini_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    /// A session header, as the Gemini CLI writes it first in every file.
    fn header(id: &str) -> String {
        format!(
            r#"{{"sessionId":"{id}","projectHash":"b57ad11f","startTime":"2026-09-10T11:25:33.119Z","lastUpdated":"2026-09-10T11:25:33.119Z","kind":"main"}}"#
        )
    }

    /// Put a session where the Gemini CLI would put it.
    fn session_file(home: &Path, project: &str, name: &str, contents: &str) -> PathBuf {
        let dir = home
            .join(PROJECTS_SUBDIRECTORY)
            .join(project)
            .join(CHATS_SUBDIRECTORY);
        fs::create_dir_all(&dir).expect("create chats directory");
        let path = dir.join(format!("{SESSION_PREFIX}{name}.{SESSION_EXTENSION}"));
        fs::write(&path, contents).expect("write session");
        path
    }

    /// Record a project's real path, as `.project_root` does.
    fn project_root(home: &Path, project: &str, path: &str) {
        let dir = home.join(PROJECTS_SUBDIRECTORY).join(project);
        fs::create_dir_all(&dir).expect("create project directory");
        fs::write(dir.join(PROJECT_ROOT_FILE), path).expect("write project root");
    }

    #[test]
    fn no_gemini_directory_means_no_sessions_not_an_error() {
        let home = gemini_home();
        let adapter = Gemini::rooted_at(home.path().join("absent"));
        assert!(adapter.discover().expect("discover").is_empty());
    }

    #[test]
    fn a_project_that_has_never_held_a_session_yields_nothing() {
        // `tmp/<project>/` exists from the moment the CLI runs there; `chats/`
        // only appears once a session is started.
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        assert!(Gemini::rooted_at(home.path())
            .discover()
            .expect("discover")
            .is_empty());
    }

    #[test]
    fn sessions_are_found_across_projects() {
        let home = gemini_home();
        project_root(home.path(), "alpha", "/w/alpha");
        project_root(home.path(), "beta", "/w/beta");
        session_file(
            home.path(),
            "alpha",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );
        session_file(
            home.path(),
            "beta",
            "2026-09-10T11-26-bbbbbbbb",
            &header("bbbbbbbb-1111-2222-3333-444444444444"),
        );

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        let mut ids: Vec<_> = found
            .iter()
            .map(|d| d.provider_session_id.clone())
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "aaaaaaaa-1111-2222-3333-444444444444",
                "bbbbbbbb-1111-2222-3333-444444444444"
            ]
        );
    }

    #[test]
    fn the_session_id_is_the_whole_one_not_the_filenames_prefix() {
        // A file named `session-<timestamp>-be9de464.jsonl` carries only the
        // first 8 characters of the id. Archiving that prefix would collide two
        // sessions that share it and would trace back to nothing.
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        session_file(
            home.path(),
            "demo",
            "2026-09-10T11-25-be9de464",
            &header("be9de464-9658-4c1b-a810-664ecc8b06e8"),
        );

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(
            found[0].provider_session_id,
            "be9de464-9658-4c1b-a810-664ecc8b06e8"
        );
    }

    #[test]
    fn the_project_is_the_path_recorded_beside_the_sessions() {
        // The directory name is only the project's last path component, so it
        // cannot be reversed into a path.
        let home = gemini_home();
        project_root(
            home.path(),
            "campuseats",
            "/Users/me/Work/STACKWARES/campuseats",
        );
        session_file(
            home.path(),
            "campuseats",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(
            found[0].project,
            Some(PathBuf::from("/Users/me/Work/STACKWARES/campuseats"))
        );
    }

    #[test]
    fn two_projects_with_the_same_name_are_told_apart() {
        // `/w/api` and `/other/api` both get a directory called `api`. Only
        // `.project_root` distinguishes them, and without it a session would be
        // archived against the wrong project.
        let home = gemini_home();
        project_root(home.path(), "api", "/w/api");
        session_file(
            home.path(),
            "api",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found[0].project, Some(PathBuf::from("/w/api")));
    }

    #[test]
    fn a_project_with_no_recorded_root_claims_none() {
        let home = gemini_home();
        session_file(
            home.path(),
            "orphan",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );
        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found[0].project, None, "a project path was invented");
    }

    #[test]
    fn a_session_whose_header_cannot_be_read_is_not_given_a_guessed_id() {
        // Unlike Codex, the filename holds no full id to fall back on, so
        // there is nothing honest to archive this under.
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        session_file(home.path(), "demo", "2026-09-10T11-25-broken", "{not json");

        assert!(Gemini::rooted_at(home.path())
            .discover()
            .expect("discover")
            .is_empty());
    }

    #[test]
    fn only_session_files_are_reported() {
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        session_file(
            home.path(),
            "demo",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );
        let chats = home
            .path()
            .join(PROJECTS_SUBDIRECTORY)
            .join("demo")
            .join(CHATS_SUBDIRECTORY);
        for stray in ["notes.md", "checkpoint.json", "session-without-extension"] {
            fs::write(chats.join(stray), b"not a session").expect("write");
        }

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 1, "found {found:?}");
    }

    #[test]
    fn the_projects_own_bookkeeping_is_not_a_session() {
        // `logs.json` and `logs/` sit beside `chats/`, not inside it, but a
        // discovery that walked the project directory would pick them up.
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        session_file(
            home.path(),
            "demo",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );
        let project = home.path().join(PROJECTS_SUBDIRECTORY).join("demo");
        fs::write(project.join("logs.json"), b"[]").expect("write");
        fs::create_dir_all(project.join("logs")).expect("mkdir");

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 1, "found {found:?}");
    }

    #[test]
    fn each_session_stands_on_its_own() {
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        session_file(
            home.path(),
            "demo",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );
        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert!(found[0].additional_paths.is_empty());
    }

    #[test]
    fn discovery_reports_a_stable_order() {
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        for (name, id) in [
            (
                "2026-09-10T11-27-zzzzzzzz",
                "zzzzzzzz-1111-2222-3333-444444444444",
            ),
            (
                "2026-09-10T11-25-aaaaaaaa",
                "aaaaaaaa-1111-2222-3333-444444444444",
            ),
            (
                "2026-09-10T11-26-mmmmmmmm",
                "mmmmmmmm-1111-2222-3333-444444444444",
            ),
        ] {
            session_file(home.path(), "demo", name, &header(id));
        }
        let adapter = Gemini::rooted_at(home.path());
        let first: Vec<_> = adapter
            .discover()
            .expect("discover")
            .iter()
            .map(|d| d.provider_session_id.clone())
            .collect();
        let again: Vec<_> = adapter
            .discover()
            .expect("discover")
            .iter()
            .map(|d| d.provider_session_id.clone())
            .collect();
        assert_eq!(first, again, "two runs disagreed about ordering");
    }

    #[test]
    fn discovery_does_not_read_the_transcript() {
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        let mut contents = header("aaaaaaaa-1111-2222-3333-444444444444");
        contents.push('\n');
        contents.push_str("\u{0}not json at all");
        session_file(home.path(), "demo", "2026-09-10T11-25-aaaaaaaa", &contents);

        let found = Gemini::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn discovery_leaves_the_provider_directory_untouched() {
        let home = gemini_home();
        project_root(home.path(), "demo", "/w/demo");
        let path = session_file(
            home.path(),
            "demo",
            "2026-09-10T11-25-aaaaaaaa",
            &header("aaaaaaaa-1111-2222-3333-444444444444"),
        );
        let before = fs::read(&path).expect("read");
        let modified_before = fs::metadata(&path).expect("metadata").modified().ok();

        Gemini::rooted_at(home.path()).discover().expect("discover");

        assert_eq!(fs::read(&path).expect("read"), before);
        assert_eq!(
            fs::metadata(&path).expect("metadata").modified().ok(),
            modified_before,
            "discovery modified the provider's file"
        );
    }
}
