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

use std::path::PathBuf;

use recall_core::{Adapter, AdapterError, DiscoveredSession, Provider, Session};

/// The provider name Claude Code sessions are recorded under.
pub const PROVIDER: &str = "claude-code";

/// Directory under the user's home where Claude Code keeps its state.
pub const HOME_SUBDIRECTORY: &str = ".claude";

/// Directory under [`HOME_SUBDIRECTORY`] holding per-project session files.
pub const PROJECTS_SUBDIRECTORY: &str = "projects";

/// Extension of a Claude Code session file.
pub const SESSION_EXTENSION: &str = "jsonl";

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
        // #20
        Ok(Vec::new())
    }

    fn load(&self, discovered: &DiscoveredSession) -> Result<Session, AdapterError> {
        // #21 parses, #22 normalizes.
        Err(AdapterError::Empty {
            provider: self.provider.clone(),
            provider_session_id: discovered.provider_session_id.clone(),
        })
    }
}

/// The user's home directory.
///
/// Resolved from the environment rather than by walking anywhere: Recall never
/// searches for a provider's files speculatively.
fn home_directory() -> Option<PathBuf> {
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

    #[test]
    fn discovery_finds_nothing_before_it_is_implemented() {
        // Deliberate: an unimplemented adapter reports no sessions rather than
        // pretending, and a sync over it archives nothing.
        let adapter = ClaudeCode::rooted_at("/somewhere/.claude");
        assert!(adapter.discover().expect("discover").is_empty());
    }
}
