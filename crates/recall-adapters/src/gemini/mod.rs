//! Gemini CLI.
//!
//! Google's open-source terminal agent, installed as `@google/gemini-cli`. It
//! keeps its state under the user's home directory:
//!
//! ```text
//! ~/.gemini/
//! ```
//!
//! On Windows the same directory sits under `%USERPROFILE%\.gemini`.
//!
//! **That directory is not the Gemini CLI's alone.** Antigravity — a different
//! Google product, invoked as `agy` — stores its own state in
//! `~/.gemini/antigravity/`, `antigravity-cli/` and `antigravity-ide/`, as
//! conversations in SQLite with protobuf payloads. Those are not Gemini CLI
//! sessions and must never be read as though they were. Discovery therefore
//! resolves an explicit subdirectory rather than treating everything under
//! `~/.gemini` as history — see [`Self::sessions_directory`] and #45.
//!
//! Verified against Gemini CLI 0.59.0. The session layout itself is #45, and
//! nothing here should be treated as settled about it until that lands.
//!
//! Parsing and normalization are #46, fixtures #47.

use std::path::PathBuf;

use recall_core::{Adapter, AdapterError, DiscoveredSession, Provider, Session};

use crate::local::home_directory;

/// The provider name Gemini CLI sessions are recorded under.
///
/// Named for the CLI rather than the model: `gemini` is a family of models that
/// several tools call, and a session belongs to the agent that produced it.
pub const PROVIDER: &str = "gemini-cli";

/// Directory under the user's home where the Gemini CLI keeps its state.
///
/// Shared with Antigravity, which is why discovery never reads it directly.
pub const HOME_SUBDIRECTORY: &str = ".gemini";

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
        // Narrowed to the Gemini CLI's own session directory once #45 has
        // established where that is. Reporting the home directory in the
        // meantime says where Recall is looking without claiming more.
        self.gemini_home().into_iter().collect()
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>, AdapterError> {
        // Walking the session tree is #45, and doing it wrongly here would mean
        // reading Antigravity's conversations as though they were the Gemini
        // CLI's. Reporting nothing is the same answer this adapter gives on a
        // machine without the Gemini CLI installed.
        Ok(Vec::new())
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
        assert_eq!(
            adapter.search_roots(),
            vec![PathBuf::from("/somewhere/.gemini")]
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
}
