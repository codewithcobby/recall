//! Claude Code.
//!
//! Sessions live at:
//!
//! ```text
//! ~/.claude/projects/<slugified-cwd>/<session-uuid>.jsonl
//! ```
//!
//! The project directory is the working directory with separators replaced by
//! `-`, so `/Users/me/work` becomes `-Users-me-work`. Other files live under
//! `projects/` too, so discovery filters on extension rather than assuming
//! everything there is a session.
//!
//! The format is documented in `docs/providers/claude-code.md`, confirmed
//! against Claude Code 2.1.215–2.1.228.
//!
//! Discovery lands in #20, parsing in #21, normalization in #22.

pub mod normalize;
pub mod parse;
pub mod record;

use std::fs;
use std::io;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use recall_core::{Adapter, AdapterError, DiscoveredSession, Provider, Session};

use crate::local::{home_directory, modified_at, read_directory};

/// The provider name Claude Code sessions are recorded under.
pub const PROVIDER: &str = "claude-code";

/// Directory under the user's home where Claude Code keeps its state.
pub const HOME_SUBDIRECTORY: &str = ".claude";

/// Directory under [`HOME_SUBDIRECTORY`] holding per-project session files.
pub const PROJECTS_SUBDIRECTORY: &str = "projects";

/// Extension of a Claude Code session file.
pub const SESSION_EXTENSION: &str = "jsonl";

/// Directory beside a session holding its sub-agent transcripts.
///
/// Claude Code stores work done by sub-agents in
/// `<session-uuid>/subagents/<task-uuid>.jsonl`. Those records carry the
/// *parent* session's `sessionId` and `isSidechain: true`, so they are part of
/// the session rather than sessions of their own.
///
/// This matters more than it looks: in the installation this adapter was
/// written against, 121 of 158 session files were sub-agent transcripts.
/// Discovering only the top-level files would have silently dropped three
/// quarters of the archived work.
pub const SUBAGENTS_SUBDIRECTORY: &str = "subagents";

/// Reads Claude Code's session history.
#[derive(Debug, Clone)]
pub struct ClaudeCode {
    provider: Provider,
    /// Where to look. Injectable so tests never touch a real home directory.
    root: Option<PathBuf>,
}

impl ClaudeCode {
    /// An adapter reading the current user's Claude Code sessions.
    pub fn new() -> Self {
        Self {
            provider: Provider::new(PROVIDER).expect("the provider name is a valid one"),
            root: None,
        }
    }

    /// An adapter reading a specific `.claude` directory.
    ///
    /// Tests use this. A test that read the real home directory would depend on
    /// whoever ran it, and would be reading someone's actual transcripts.
    pub fn rooted_at(claude_home: impl Into<PathBuf>) -> Self {
        Self {
            provider: Provider::new(PROVIDER).expect("the provider name is a valid one"),
            root: Some(claude_home.into()),
        }
    }

    /// The `.claude` directory this adapter reads, if it can be determined.
    pub fn claude_home(&self) -> Option<PathBuf> {
        match &self.root {
            Some(root) => Some(root.clone()),
            None => home_directory().map(|home| home.join(HOME_SUBDIRECTORY)),
        }
    }

    /// The directory holding per-project session directories.
    pub fn projects_directory(&self) -> Option<PathBuf> {
        self.claude_home()
            .map(|home| home.join(PROJECTS_SUBDIRECTORY))
    }
}

impl Default for ClaudeCode {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for ClaudeCode {
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
                // projects/ holds stray files too. They are not sessions.
                continue;
            }
            let project = project_from_directory_name(&project_dir);

            for entry in read_directory(&project_dir)? {
                if entry.extension().and_then(|e| e.to_str()) != Some(SESSION_EXTENSION) {
                    continue;
                }
                if !entry.is_file() {
                    continue;
                }
                let Some(id) = entry.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };

                found.push(DiscoveredSession {
                    provider_session_id: id.to_string(),
                    modified: modified_at(&entry),
                    // The exact working directory, falling back to the lossy
                    // one decoded from the directory name.
                    project: recorded_project(&entry).or_else(|| project.clone()),
                    additional_paths: subagent_transcripts(&project_dir, id)?,
                    path: entry,
                });
            }
        }

        // Stable order, so two runs report the same thing.
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    fn load(&self, discovered: &DiscoveredSession) -> Result<Session, AdapterError> {
        let mut files = Vec::with_capacity(1 + discovered.additional_paths.len());
        for path in std::iter::once(&discovered.path).chain(discovered.additional_paths.iter()) {
            files.push(parse::parse_file(
                &self.provider,
                &discovered.provider_session_id,
                path,
            )?);
        }

        normalize::normalize(
            &self.provider,
            &discovered.provider_session_id,
            discovered.project.clone(),
            &files,
        )
    }

    fn provider_version(&self, path: &Path) -> Option<String> {
        // The version is on every content record; the first one will do.
        let parsed = parse::parse_file(&self.provider, "", path).ok()?;
        parsed.records.iter().find_map(|r| r.version.clone())
    }
}

/// Sub-agent transcripts belonging to one session.
///
/// They live in `<project>/<session-uuid>/subagents/`, and their records carry
/// the parent session's id, so they are part of that session rather than
/// sessions in their own right.
fn subagent_transcripts(
    project_dir: &Path,
    session_id: &str,
) -> Result<Vec<PathBuf>, AdapterError> {
    let dir = project_dir.join(session_id).join(SUBAGENTS_SUBDIRECTORY);
    let mut found: Vec<PathBuf> = read_directory(&dir)?
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(SESSION_EXTENSION))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    Ok(found)
}

/// How many lines to read looking for the working directory.
///
/// Every content record carries `cwd`, but a session can open with Claude
/// Code's own bookkeeping, so this reads past a reasonable number of those
/// before giving up. Small enough that discovery stays cheap over hundreds of
/// files.
const CWD_SEARCH_LINES: usize = 64;

/// The working directory recorded inside a session.
///
/// This is exact, unlike the path decoded from the directory name — which
/// cannot round-trip a project whose own path contains a hyphen, and silently
/// produced the wrong directory for every such project (#115).
///
/// Reads only the opening lines, not the transcript.
fn recorded_project(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let reader = io::BufReader::new(file);

    for line in reader.lines().take(CWD_SEARCH_LINES) {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Deliberately not the full record type: this needs one field, and
        // the cheapest possible read of it.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(|c| c.as_str()) {
            if !cwd.is_empty() {
                return Some(PathBuf::from(cwd));
            }
        }
    }
    None
}

/// Recover the project path from a slugified directory name.
///
/// Claude Code encodes the working directory by replacing separators with `-`,
/// so `/Users/me/work` becomes `-Users-me-work`. That transformation loses
/// information: a directory whose real name contains a hyphen is
/// indistinguishable from a separator, so `-Users-me-my-project` could be
/// `/Users/me/my/project` or `/Users/me/my-project`.
///
/// Recall therefore reports the decoded path as a best-effort hint and does not
/// treat it as authoritative. The `cwd` recorded inside the session is exact,
/// and #22 uses that instead. Nothing here is ever opened.
fn project_from_directory_name(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    if !name.starts_with('-') {
        return None;
    }
    Some(PathBuf::from(name.replace('-', "/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_is_named_consistently() {
        let adapter = ClaudeCode::new();
        assert_eq!(adapter.provider().as_str(), "claude-code");
    }

    #[test]
    fn a_rooted_adapter_looks_only_where_it_was_told() {
        let adapter = ClaudeCode::rooted_at("/somewhere/.claude");
        assert_eq!(
            adapter.projects_directory(),
            Some(PathBuf::from("/somewhere/.claude/projects"))
        );
        assert_eq!(
            adapter.search_roots(),
            vec![PathBuf::from("/somewhere/.claude/projects")]
        );
    }

    /// A fake Claude Code home. No test touches the real one.
    fn claude_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    /// Put a session file where Claude Code would put it.
    fn session_file(home: &Path, project: &str, id: &str, contents: &str) -> PathBuf {
        let dir = home.join(PROJECTS_SUBDIRECTORY).join(project);
        fs::create_dir_all(&dir).expect("create project directory");
        let path = dir.join(format!("{id}.{SESSION_EXTENSION}"));
        fs::write(&path, contents).expect("write session");
        path
    }

    #[test]
    fn no_claude_directory_means_no_sessions_not_an_error() {
        // Claude Code is simply not installed. A sync over this archives
        // nothing and carries on to the next provider.
        let home = claude_home();
        let adapter = ClaudeCode::rooted_at(home.path().join("absent"));
        assert!(adapter.discover().expect("discover").is_empty());
    }

    #[test]
    fn an_empty_projects_directory_yields_nothing() {
        let home = claude_home();
        fs::create_dir_all(home.path().join(PROJECTS_SUBDIRECTORY)).expect("mkdir");
        let adapter = ClaudeCode::rooted_at(home.path());
        assert!(adapter.discover().expect("discover").is_empty());
    }

    #[test]
    fn session_files_are_found_across_projects() {
        let home = claude_home();
        session_file(home.path(), "-Users-me-alpha", "aaaa-1111", "{}\n");
        session_file(home.path(), "-Users-me-beta", "bbbb-2222", "{}\n");
        session_file(home.path(), "-Users-me-beta", "cccc-3333", "{}\n");

        let adapter = ClaudeCode::rooted_at(home.path());
        let found = adapter.discover().expect("discover");

        let mut ids: Vec<_> = found
            .iter()
            .map(|d| d.provider_session_id.clone())
            .collect();
        ids.sort();
        assert_eq!(ids, ["aaaa-1111", "bbbb-2222", "cccc-3333"]);
    }

    #[test]
    fn only_session_files_are_reported() {
        // projects/ holds other things: transcripts exported to markdown,
        // scratch json, notes. None of them are sessions.
        let home = claude_home();
        session_file(home.path(), "-Users-me-alpha", "real-session", "{}\n");
        let dir = home
            .path()
            .join(PROJECTS_SUBDIRECTORY)
            .join("-Users-me-alpha");
        for stray in ["notes.md", "scratch.json", "output.txt", "no-extension"] {
            fs::write(dir.join(stray), b"not a session").expect("write");
        }
        fs::create_dir_all(dir.join("subdir.jsonl")).expect("a directory that looks like one");

        let adapter = ClaudeCode::rooted_at(home.path());
        let found = adapter.discover().expect("discover");
        assert_eq!(found.len(), 1, "found {found:?}");
        assert_eq!(found[0].provider_session_id, "real-session");
    }

    #[test]
    fn stray_files_directly_under_projects_are_ignored() {
        let home = claude_home();
        let projects = home.path().join(PROJECTS_SUBDIRECTORY);
        fs::create_dir_all(&projects).expect("mkdir");
        fs::write(projects.join("stray.jsonl"), b"not in a project").expect("write");
        session_file(home.path(), "-Users-me-alpha", "real", "{}\n");

        let adapter = ClaudeCode::rooted_at(home.path());
        let found = adapter.discover().expect("discover");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider_session_id, "real");
    }

    #[test]
    fn the_project_path_is_decoded_from_the_directory_name() {
        let home = claude_home();
        session_file(home.path(), "-Users-me-Documents-Work", "abc", "{}\n");

        let adapter = ClaudeCode::rooted_at(home.path());
        let found = adapter.discover().expect("discover");
        assert_eq!(
            found[0].project,
            Some(PathBuf::from("/Users/me/Documents/Work"))
        );
    }

    #[test]
    fn the_project_path_comes_from_the_session_not_the_directory_name() {
        // The decoded slug cannot round-trip a path containing a hyphen. Before
        // this, every project whose own path had one was silently skipped by
        // sync: discovery reported the wrong directory and nothing matched.
        let home = claude_home();
        session_file(
            home.path(),
            "-Users-me-OPEN-SOURCE-recall",
            "abc",
            "{\"type\":\"user\",\"cwd\":\"/Users/me/OPEN-SOURCE/recall\",\"message\":{\"content\":\"x\"}}\n",
        );

        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        assert_eq!(
            found[0].project,
            Some(PathBuf::from("/Users/me/OPEN-SOURCE/recall")),
            "the lossy directory name was used instead of the recorded cwd"
        );
    }

    #[test]
    fn the_directory_name_is_used_when_no_record_supplies_a_cwd() {
        let home = claude_home();
        session_file(
            home.path(),
            "-Users-me-plain",
            "abc",
            "{\"type\":\"ai-title\",\"aiTitle\":\"x\"}\n",
        );
        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        assert_eq!(found[0].project, Some(PathBuf::from("/Users/me/plain")));
    }

    #[test]
    fn a_cwd_is_found_past_leading_bookkeeping_records() {
        let home = claude_home();
        let mut lines = String::new();
        for _ in 0..20 {
            lines.push_str("{\"type\":\"mode\",\"mode\":\"normal\"}\n");
        }
        lines.push_str("{\"type\":\"user\",\"cwd\":\"/w/real\",\"message\":{\"content\":\"x\"}}\n");
        session_file(home.path(), "-w-real", "abc", &lines);

        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        assert_eq!(found[0].project, Some(PathBuf::from("/w/real")));
    }

    #[test]
    fn an_unparseable_opening_line_does_not_stop_the_search() {
        let home = claude_home();
        session_file(
            home.path(),
            "-w-real",
            "abc",
            "{not json at all\n{\"type\":\"user\",\"cwd\":\"/w/real\",\"message\":{\"content\":\"x\"}}\n",
        );
        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        assert_eq!(found[0].project, Some(PathBuf::from("/w/real")));
    }

    #[test]
    fn a_directory_name_that_is_not_a_slug_yields_no_project() {
        let home = claude_home();
        session_file(home.path(), "not-a-slug", "abc", "{}\n");
        let adapter = ClaudeCode::rooted_at(home.path());
        assert_eq!(adapter.discover().expect("discover")[0].project, None);
    }

    #[test]
    fn discovery_reports_a_stable_order() {
        let home = claude_home();
        for (project, id) in [
            ("-Users-me-beta", "zzz"),
            ("-Users-me-alpha", "aaa"),
            ("-Users-me-alpha", "mmm"),
        ] {
            session_file(home.path(), project, id, "{}\n");
        }
        let adapter = ClaudeCode::rooted_at(home.path());
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
    fn discovery_does_not_read_session_contents() {
        // A file that would fail any parser must still be discovered: finding
        // it and understanding it are separate steps, and a sync needs the
        // first to work even when the second will not.
        let home = claude_home();
        session_file(
            home.path(),
            "-Users-me-alpha",
            "unreadable",
            "\u{0}not json at all",
        );
        let adapter = ClaudeCode::rooted_at(home.path());
        assert_eq!(adapter.discover().expect("discover").len(), 1);
    }

    /// Put a sub-agent transcript where Claude Code would put it.
    fn subagent_file(home: &Path, project: &str, session: &str, task: &str) -> PathBuf {
        let dir = home
            .join(PROJECTS_SUBDIRECTORY)
            .join(project)
            .join(session)
            .join(SUBAGENTS_SUBDIRECTORY);
        fs::create_dir_all(&dir).expect("create subagents directory");
        let path = dir.join(format!("{task}.{SESSION_EXTENSION}"));
        fs::write(&path, "{}\n").expect("write subagent transcript");
        path
    }

    #[test]
    fn subagent_transcripts_belong_to_their_parent_session() {
        // Their records carry the parent's sessionId and isSidechain: true, so
        // they are part of that session rather than sessions of their own.
        // Reporting them as peers would scatter one conversation across four.
        let home = claude_home();
        session_file(home.path(), "-Users-me-alpha", "parent-1", "{}\n");
        subagent_file(home.path(), "-Users-me-alpha", "parent-1", "task-a");
        subagent_file(home.path(), "-Users-me-alpha", "parent-1", "task-b");

        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");

        assert_eq!(found.len(), 1, "sub-agents were reported as sessions");
        assert_eq!(found[0].provider_session_id, "parent-1");
        assert_eq!(found[0].additional_paths.len(), 2);
        assert!(found[0]
            .additional_paths
            .iter()
            .all(|p| p.to_string_lossy().contains(SUBAGENTS_SUBDIRECTORY)));
    }

    #[test]
    fn a_session_without_subagents_has_none_attached() {
        let home = claude_home();
        session_file(home.path(), "-Users-me-alpha", "solo", "{}\n");
        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        assert!(found[0].additional_paths.is_empty());
    }

    #[test]
    fn subagent_directories_are_not_mistaken_for_projects() {
        // <session-uuid>/ sits inside the project directory beside the session
        // file. It must not be walked as though it were a project of its own.
        let home = claude_home();
        session_file(home.path(), "-Users-me-alpha", "parent-1", "{}\n");
        subagent_file(home.path(), "-Users-me-alpha", "parent-1", "task-a");

        let found = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        let ids: Vec<_> = found.iter().map(|d| &d.provider_session_id).collect();
        assert_eq!(ids, ["parent-1"], "found {ids:?}");
    }

    #[test]
    fn subagent_transcripts_are_attached_in_a_stable_order() {
        let home = claude_home();
        session_file(home.path(), "-Users-me-alpha", "parent-1", "{}\n");
        for task in ["zzz", "aaa", "mmm"] {
            subagent_file(home.path(), "-Users-me-alpha", "parent-1", task);
        }
        let a = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        let b = ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");
        assert_eq!(a[0].additional_paths, b[0].additional_paths);
    }

    #[test]
    fn discovery_leaves_the_provider_directory_untouched() {
        let home = claude_home();
        let path = session_file(home.path(), "-Users-me-alpha", "abc", "original contents\n");
        let before = fs::read(&path).expect("read");
        let modified_before = fs::metadata(&path).expect("metadata").modified().ok();

        ClaudeCode::rooted_at(home.path())
            .discover()
            .expect("discover");

        assert_eq!(fs::read(&path).expect("read"), before);
        assert_eq!(
            fs::metadata(&path).expect("metadata").modified().ok(),
            modified_before,
            "discovery modified the provider's file"
        );
    }
}
