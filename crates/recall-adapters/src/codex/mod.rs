//! Codex CLI.
//!
//! Sessions live in a date-partitioned tree under the user's home directory:
//!
//! ```text
//! ~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<timestamp>-<session-uuid>.jsonl
//! ```
//!
//! On Windows the same tree sits under `%USERPROFILE%\.codex`. Codex calls
//! these files *rollouts*; one file is one session, and unlike Claude Code
//! there are no sibling transcripts to attach.
//!
//! The tree is partitioned by the date the session started, **not** by project.
//! Nothing in the path says which repository a session ran against — that comes
//! from the `cwd` recorded inside the file, which is why discovery has to read
//! a little of each one (#41).
//!
//! Verified against Codex CLI 0.152.1. The record format is documented in
//! `docs/providers/codex.md`.
//!
//! Fixtures and their tests land in #43.

pub mod normalize;
pub mod parse;
pub mod record;

use std::fs;
use std::io;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use recall_core::{Adapter, AdapterError, DiscoveredSession, Provider, Session};

use crate::local::{home_directory, modified_at, read_directory};

/// The provider name Codex sessions are recorded under.
pub const PROVIDER: &str = "codex";

/// Directory under the user's home where Codex keeps its state.
pub const HOME_SUBDIRECTORY: &str = ".codex";

/// Directory under [`HOME_SUBDIRECTORY`] holding the date-partitioned rollouts.
pub const SESSIONS_SUBDIRECTORY: &str = "sessions";

/// Extension of a Codex rollout file.
pub const SESSION_EXTENSION: &str = "jsonl";

/// Filename prefix Codex gives a rollout.
pub const ROLLOUT_PREFIX: &str = "rollout-";

/// Reads Codex CLI's session history.
#[derive(Debug, Clone)]
pub struct Codex {
    provider: Provider,
    /// Where to look. Injectable so tests never touch a real home directory.
    root: Option<PathBuf>,
}

impl Codex {
    /// An adapter reading the current user's Codex sessions.
    pub fn new() -> Self {
        Self {
            provider: Provider::new(PROVIDER).expect("the provider name is a valid one"),
            root: None,
        }
    }

    /// An adapter reading a specific `.codex` directory.
    ///
    /// Tests use this. A test that read the real home directory would depend on
    /// whoever ran it, and would be reading someone's actual transcripts.
    pub fn rooted_at(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            provider: Provider::new(PROVIDER).expect("the provider name is a valid one"),
            root: Some(codex_home.into()),
        }
    }

    /// The `.codex` directory this adapter reads, if it can be determined.
    pub fn codex_home(&self) -> Option<PathBuf> {
        match &self.root {
            Some(root) => Some(root.clone()),
            None => home_directory().map(|home| home.join(HOME_SUBDIRECTORY)),
        }
    }

    /// The root of the date-partitioned session tree.
    pub fn sessions_directory(&self) -> Option<PathBuf> {
        self.codex_home()
            .map(|home| home.join(SESSIONS_SUBDIRECTORY))
    }
}

impl Default for Codex {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for Codex {
    fn provider(&self) -> &Provider {
        &self.provider
    }

    fn search_roots(&self) -> Vec<PathBuf> {
        self.sessions_directory().into_iter().collect()
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>, AdapterError> {
        let Some(sessions) = self.sessions_directory() else {
            // No home directory to resolve. Not an error: there is simply
            // nothing to read.
            return Ok(Vec::new());
        };

        let mut rollouts = Vec::new();
        collect_rollouts(&sessions, 0, &mut rollouts)?;

        let mut found = Vec::new();
        for path in rollouts {
            let opening = opening_metadata(&path);

            // The id recorded inside the file is authoritative. The one in the
            // filename is a fallback for a rollout whose header is missing or
            // unreadable — discovery must still report such a file, because
            // finding it and understanding it are separate steps.
            let Some(id) = opening
                .session_id
                .or_else(|| session_id_from_filename(&path))
            else {
                continue;
            };

            found.push(DiscoveredSession {
                provider_session_id: id,
                modified: modified_at(&path),
                project: opening.cwd,
                // Codex keeps one file per session. There is no sibling
                // transcript to attach, as there is for Claude Code.
                additional_paths: Vec::new(),
                path,
            });
        }

        // Stable order, so two runs report the same thing.
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    fn load(&self, discovered: &DiscoveredSession) -> Result<Session, AdapterError> {
        // One rollout is one session, so unlike Claude Code there is nothing to
        // merge — `additional_paths` is always empty for this provider.
        let parsed = parse::parse_file(
            &self.provider,
            &discovered.provider_session_id,
            &discovered.path,
        )?;

        normalize::normalize(
            &self.provider,
            &discovered.provider_session_id,
            discovered.project.clone(),
            &parsed,
        )
    }

    fn provider_version(&self, path: &Path) -> Option<String> {
        // Recorded once, on the session header.
        let parsed = parse::parse_file(&self.provider, "", path).ok()?;
        parsed
            .records
            .iter()
            .find_map(|r| r.session_meta()?.cli_version)
    }
}

/// How deep below `sessions/` a rollout may sit.
///
/// The layout is `<YYYY>/<MM>/<DD>/`, so three. One extra level is allowed so a
/// future partitioning change costs a review rather than the user's archive,
/// and the bound is what stops discovery wandering off through a symlink or an
/// unexpectedly deep tree.
const MAX_DEPTH: usize = 4;

/// Collect rollout files from the date-partitioned tree.
///
/// Directories are descended to [`MAX_DEPTH`]; symlinks are not followed, so a
/// link planted under `sessions/` cannot walk Recall out of the tree it was
/// told to read.
fn collect_rollouts(
    dir: &Path,
    depth: usize,
    found: &mut Vec<PathBuf>,
) -> Result<(), AdapterError> {
    for entry in read_directory(dir)? {
        // symlink_metadata, not metadata: this must describe the link itself
        // rather than whatever it points at.
        let Ok(meta) = fs::symlink_metadata(&entry) else {
            // Vanished between listing and stat, or unreadable. One entry is
            // not worth failing the sync over.
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            if depth < MAX_DEPTH {
                collect_rollouts(&entry, depth + 1, found)?;
            }
            continue;
        }

        if is_rollout(&entry) {
            found.push(entry);
        }
    }
    Ok(())
}

/// Whether a file is one of Codex's rollouts.
///
/// Filtered on both the prefix and the extension: the sessions tree is Codex's
/// own, and anything else that appears there is not a session.
fn is_rollout(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some(SESSION_EXTENSION) {
        return false;
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(ROLLOUT_PREFIX))
}

/// What the opening records of a rollout say about the session.
#[derive(Debug, Default)]
struct OpeningMetadata {
    /// Codex's own identifier for the session.
    session_id: Option<String>,
    /// The working directory the session ran in.
    cwd: Option<PathBuf>,
}

/// How many lines to read looking for the session header.
///
/// Codex writes `session_meta` first, so in practice this reads one line. The
/// allowance is for a rollout that opens with something else, and is small
/// enough that discovery stays cheap over a whole history.
const HEADER_SEARCH_LINES: usize = 8;

/// Read what the opening records say, without parsing the transcript.
///
/// The session id and working directory both come from the `session_meta`
/// record Codex writes first. Nothing in the path says which project a session
/// belongs to, so unlike Claude Code there is no directory name to fall back
/// on — this is the only source.
///
/// Only the opening lines are read, never the transcript: discovery reports
/// what exists, and `load` is what understands it.
fn opening_metadata(path: &Path) -> OpeningMetadata {
    let mut meta = OpeningMetadata::default();
    let Ok(file) = fs::File::open(path) else {
        return meta;
    };

    for line in io::BufReader::new(file).lines().take(HEADER_SEARCH_LINES) {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<record::Record>(line) else {
            continue;
        };
        let Some(header) = record.session_meta() else {
            continue;
        };

        meta.session_id = header.identifier().map(str::to_string);
        meta.cwd = header.cwd.filter(|c| !c.is_empty()).map(PathBuf::from);
        break;
    }

    meta
}

/// How many hyphen-separated groups a uuid has.
const UUID_GROUPS: usize = 5;

/// Recover the session id from a rollout's filename.
///
/// A rollout is named `rollout-<timestamp>-<session-uuid>.jsonl`, and the
/// timestamp contains hyphens too (`2026-08-03T13-10-33`), so the uuid is
/// recovered as the trailing five groups rather than by splitting once.
///
/// Only used when the file's own header could not be read. The id inside the
/// file is the one Codex considers authoritative, and this reconstruction
/// cannot be checked against anything.
fn session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let rest = stem.strip_prefix(ROLLOUT_PREFIX)?;

    let groups: Vec<&str> = rest.split('-').collect();
    if groups.len() >= UUID_GROUPS {
        let candidate = groups[groups.len() - UUID_GROUPS..].join("-");
        if looks_like_uuid(&candidate) {
            return Some(candidate);
        }
    }

    // Named like a rollout but not shaped like one. Reporting the whole
    // remainder keeps the session discoverable instead of dropping it.
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Whether text is shaped like a uuid: 8-4-4-4-12 hex digits.
fn looks_like_uuid(text: &str) -> bool {
    const GROUP_LENGTHS: [usize; UUID_GROUPS] = [8, 4, 4, 4, 12];

    let mut groups = text.split('-');
    for expected in GROUP_LENGTHS {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != expected || !group.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
    }
    groups.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_is_named_consistently() {
        assert_eq!(Codex::new().provider().as_str(), "codex");
    }

    #[test]
    fn codex_and_claude_sessions_are_never_confused() {
        // The provider is part of a session's derived identity, so two agents
        // that happen to use the same uuid still archive as separate sessions.
        use recall_core::SessionId;
        let codex = Codex::new();
        let claude = crate::ClaudeCode::new();
        assert_ne!(
            SessionId::derive(codex.provider(), "shared-id"),
            SessionId::derive(claude.provider(), "shared-id"),
        );
    }

    #[test]
    fn a_rooted_adapter_looks_only_where_it_was_told() {
        let adapter = Codex::rooted_at("/somewhere/.codex");
        assert_eq!(
            adapter.sessions_directory(),
            Some(PathBuf::from("/somewhere/.codex/sessions"))
        );
        assert_eq!(
            adapter.search_roots(),
            vec![PathBuf::from("/somewhere/.codex/sessions")]
        );
    }

    #[test]
    fn the_search_root_is_reported_so_a_caller_can_say_what_was_looked_at() {
        // `recall sync` prints these when it finds nothing, so a user can tell
        // "not installed" from "looked in the wrong place".
        assert!(!Codex::rooted_at("/somewhere/.codex")
            .search_roots()
            .is_empty());
    }

    /// A fake Codex home. No test touches the real one.
    fn codex_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    /// Put a rollout where Codex would put it.
    fn rollout(home: &Path, date: (&str, &str, &str), name: &str, contents: &str) -> PathBuf {
        let dir = home
            .join(SESSIONS_SUBDIRECTORY)
            .join(date.0)
            .join(date.1)
            .join(date.2);
        fs::create_dir_all(&dir).expect("create date directory");
        let path = dir.join(format!("{ROLLOUT_PREFIX}{name}.{SESSION_EXTENSION}"));
        fs::write(&path, contents).expect("write rollout");
        path
    }

    /// A `session_meta` line, as Codex writes it first in every rollout.
    fn session_meta(id: &str, cwd: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-03T13:11:04.855Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"{id}","id":"{id}","cwd":"{cwd}","cli_version":"0.152.1"}}}}"#
        )
    }

    #[test]
    fn no_codex_directory_means_no_sessions_not_an_error() {
        // Codex is simply not installed. A sync over this archives nothing and
        // carries on to the next provider.
        let home = codex_home();
        let adapter = Codex::rooted_at(home.path().join("absent"));
        assert!(adapter.discover().expect("discover").is_empty());
    }

    #[test]
    fn an_empty_sessions_directory_yields_nothing() {
        let home = codex_home();
        fs::create_dir_all(home.path().join(SESSIONS_SUBDIRECTORY)).expect("mkdir");
        let adapter = Codex::rooted_at(home.path());
        assert!(adapter.discover().expect("discover").is_empty());
    }

    #[test]
    fn rollouts_are_found_across_the_date_tree() {
        let home = codex_home();
        for (date, id) in [
            (("2026", "08", "03"), "019fc7bf-5907-7222-a195-fa7ee3f9556f"),
            (("2026", "09", "01"), "019fd000-0000-7000-8000-000000000001"),
            (("2025", "10", "01"), "01999ec4-bd98-7c60-b561-5762ac84b78c"),
        ] {
            rollout(
                home.path(),
                date,
                &format!("2026-08-03T13-10-33-{id}"),
                &session_meta(id, "/w/project"),
            );
        }

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 3, "found {found:?}");
    }

    #[test]
    fn the_session_id_comes_from_the_file_not_the_filename() {
        // The filename is Codex's presentation of the id; the header is the id
        // itself. They agree today, and the header wins if they ever stop.
        let home = codex_home();
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &session_meta("recorded-id", "/w/project"),
        );

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found[0].provider_session_id, "recorded-id");
    }

    #[test]
    fn the_filename_supplies_the_id_when_the_header_cannot() {
        // A rollout truncated mid-write has no readable header. Dropping it
        // would lose the session outright, so the filename stands in.
        let home = codex_home();
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            "{\"type\":\"session_me",
        );

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].provider_session_id, "aaaaaaaa-1111-2222-3333-444444444444",
            "the timestamp's own hyphens were mistaken for uuid separators"
        );
    }

    #[test]
    fn the_project_is_the_working_directory_recorded_in_the_session() {
        // Nothing in a Codex path says which project a session ran against —
        // the tree is partitioned by date. The header is the only source.
        let home = codex_home();
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &session_meta("s1", "/Users/me/OPEN-SOURCE/recall"),
        );

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(
            found[0].project,
            Some(PathBuf::from("/Users/me/OPEN-SOURCE/recall"))
        );
    }

    #[test]
    fn a_session_without_a_recorded_project_claims_none() {
        let home = codex_home();
        let dir = home.path().join(SESSIONS_SUBDIRECTORY).join("2026/08/03");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(
            dir.join("rollout-2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444.jsonl"),
            r#"{"type":"session_meta","payload":{"session_id":"s1"}}"#,
        )
        .expect("write");

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found[0].project, None, "a missing cwd was invented");
    }

    #[test]
    fn only_rollouts_are_reported() {
        // The sessions tree is Codex's own, and it puts other things there.
        let home = codex_home();
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &session_meta("real", "/w"),
        );
        let dir = home.path().join(SESSIONS_SUBDIRECTORY).join("2026/08/03");
        for stray in ["notes.md", "index.json", "rollout-without-extension"] {
            fs::write(dir.join(stray), b"not a session").expect("write");
        }

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 1, "found {found:?}");
        assert_eq!(found[0].provider_session_id, "real");
    }

    #[test]
    fn each_rollout_is_one_session_on_its_own() {
        // Unlike Claude Code, Codex keeps no sibling transcripts, so nothing
        // should ever be attached to a discovered session.
        let home = codex_home();
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &session_meta("s1", "/w"),
        );
        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert!(found[0].additional_paths.is_empty());
    }

    #[test]
    fn discovery_does_not_wander_past_the_sessions_tree() {
        // A symlink under sessions/ must not walk Recall out of the directory
        // it was told to read.
        let home = codex_home();
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &session_meta("real", "/w"),
        );

        let outside = codex_home();
        let elsewhere = outside.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("mkdir");
        fs::write(
            elsewhere
                .join("rollout-2026-08-03T13-10-33-bbbbbbbb-1111-2222-3333-444444444444.jsonl"),
            session_meta("escaped", "/w"),
        )
        .expect("write");

        #[cfg(unix)]
        std::os::unix::fs::symlink(
            &elsewhere,
            home.path().join(SESSIONS_SUBDIRECTORY).join("2026/link"),
        )
        .expect("symlink");

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        let ids: Vec<_> = found.iter().map(|d| &d.provider_session_id).collect();
        assert_eq!(
            ids,
            ["real"],
            "discovery followed a symlink out of the tree"
        );
    }

    #[test]
    fn discovery_reports_a_stable_order() {
        let home = codex_home();
        for (date, id) in [
            (("2026", "09", "01"), "zzzzzzzz-1111-2222-3333-444444444444"),
            (("2025", "10", "01"), "aaaaaaaa-1111-2222-3333-444444444444"),
            (("2026", "08", "03"), "mmmmmmmm-1111-2222-3333-444444444444"),
        ] {
            rollout(
                home.path(),
                date,
                &format!("2026-08-03T13-10-33-{id}"),
                &session_meta(id, "/w"),
            );
        }
        let adapter = Codex::rooted_at(home.path());
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
        // A rollout that would fail any parser must still be discovered:
        // finding it and understanding it are separate steps.
        let home = codex_home();
        let mut contents = session_meta("s1", "/w");
        contents.push('\n');
        contents.push_str("\u{0}not json at all");
        rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &contents,
        );

        let found = Codex::rooted_at(home.path()).discover().expect("discover");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider_session_id, "s1");
    }

    #[test]
    fn discovery_leaves_the_provider_directory_untouched() {
        let home = codex_home();
        let path = rollout(
            home.path(),
            ("2026", "08", "03"),
            "2026-08-03T13-10-33-aaaaaaaa-1111-2222-3333-444444444444",
            &session_meta("s1", "/w"),
        );
        let before = fs::read(&path).expect("read");
        let modified_before = fs::metadata(&path).expect("metadata").modified().ok();

        Codex::rooted_at(home.path()).discover().expect("discover");

        assert_eq!(fs::read(&path).expect("read"), before);
        assert_eq!(
            fs::metadata(&path).expect("metadata").modified().ok(),
            modified_before,
            "discovery modified the provider's file"
        );
    }

    #[test]
    fn a_uuid_is_recognised_only_when_it_is_shaped_like_one() {
        assert!(looks_like_uuid("019fc7bf-5907-7222-a195-fa7ee3f9556f"));
        assert!(!looks_like_uuid("019fc7bf-5907-7222-a195"));
        assert!(!looks_like_uuid(
            "019fc7bf-5907-7222-a195-fa7ee3f9556f-extra"
        ));
        assert!(!looks_like_uuid("zzzzzzzz-5907-7222-a195-fa7ee3f9556f"));
        assert!(!looks_like_uuid(""));
    }
}
