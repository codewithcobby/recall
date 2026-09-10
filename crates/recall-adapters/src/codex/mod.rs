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
//! This module is the boundary only: discovery lands in #41, parsing and
//! normalization in #42, fixtures in #43.

use std::path::PathBuf;

use recall_core::{Adapter, AdapterError, DiscoveredSession, Provider, Session};

use crate::local::home_directory;

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
        // Walking the tree is #41. Reporting nothing is the same answer this
        // adapter gives on a machine without Codex installed, so a sync over it
        // is a no-op rather than a failure.
        Ok(Vec::new())
    }

    fn load(&self, discovered: &DiscoveredSession) -> Result<Session, AdapterError> {
        // Parsing is #42. Until then `discover` reports nothing, so nothing
        // this adapter found can reach here; a caller that constructs a
        // `DiscoveredSession` by hand is told plainly that there is no session
        // to be had rather than being handed an invented one.
        Err(AdapterError::Empty {
            provider: self.provider.clone(),
            provider_session_id: discovered.provider_session_id.clone(),
        })
    }
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
}
