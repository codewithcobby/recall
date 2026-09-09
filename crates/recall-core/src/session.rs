//! What a session is, independent of the agent that produced it.
//!
//! Everything downstream plugs into these types: adapters normalize into them,
//! the archive stores them, the index summarizes them. Nothing here refers to a
//! particular AI coding agent.
//!
//! Absence is modelled, never guessed. A field a provider cannot supply is
//! `Option` and stays `None` — a session must not claim more than its source
//! actually contained.

use std::fmt;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Recall's identifier for a session.
///
/// Not the provider's id. Provider ids are unusable as filenames: they may
/// contain path separators, characters Windows rejects, or differ only by case,
/// which collides on a case-insensitive filesystem. This is derived from the
/// provider and its id, and is always 32 lowercase hex characters — safe on
/// every supported platform, and stable across machines and runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

/// Changing this changes every session id, so it is versioned deliberately.
const ID_DERIVATION_DOMAIN: &str = "recall-session-v1";

/// Bytes of digest kept. 16 bytes is 128 bits — collision-free in practice for
/// any realistic archive, and short enough to type.
const ID_BYTES: usize = 16;

impl SessionId {
    /// Derive the id for a provider's session.
    ///
    /// Deterministic: the same inputs always produce the same id, which is what
    /// lets `recall sync` recognise a session it has already archived.
    pub fn derive(provider: &Provider, provider_session_id: &str) -> Self {
        let mut hasher = Sha256::new();
        // Separators, so ("ab", "c") and ("a", "bc") cannot collide.
        hasher.update(ID_DERIVATION_DOMAIN.as_bytes());
        hasher.update([0]);
        hasher.update(provider.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(provider_session_id.as_bytes());

        let digest = hasher.finalize();
        let mut s = String::with_capacity(ID_BYTES * 2);
        for byte in &digest[..ID_BYTES] {
            use fmt::Write as _;
            let _ = write!(s, "{byte:02x}");
        }
        Self(s)
    }

    /// The id as it appears in a filename or on screen.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a provider name was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProviderError {
    /// Provider names appear in derived ids and in output; empty is not usable.
    #[error("provider name is empty")]
    Empty,

    /// Kept short because it appears in listings beside every session.
    #[error("provider name is longer than {max} characters: {name:?}")]
    TooLong { name: String, max: usize },

    /// Restricted so a provider name can never widen what an id or a path can
    /// contain.
    #[error(
        "provider name {name:?} contains {character:?}; use lowercase letters, digits and hyphens"
    )]
    InvalidCharacter { name: String, character: char },
}

/// The AI coding agent a session came from.
///
/// A validated name rather than an enum on purpose. An enum in this crate would
/// mean adding a variant here for every new adapter, which is exactly the
/// provider knowledge the core is supposed to be free of.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Provider(String);

impl Provider {
    /// Longest accepted provider name.
    pub const MAX_LEN: usize = 32;

    /// Validate and wrap a provider name.
    ///
    /// Lowercase letters, digits and hyphens only, so the name is safe wherever
    /// it ends up: a derived id, a path, a database column, a CLI flag.
    pub fn new(name: impl Into<String>) -> Result<Self, ProviderError> {
        let name = name.into();
        if name.is_empty() {
            return Err(ProviderError::Empty);
        }
        if name.len() > Self::MAX_LEN {
            return Err(ProviderError::TooLong {
                name,
                max: Self::MAX_LEN,
            });
        }
        if let Some(character) = name
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
        {
            return Err(ProviderError::InvalidCharacter { name, character });
        }
        Ok(Self(name))
    }

    /// The provider name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The repository state a session ran against.
///
/// Every field is optional: a session may happen outside a repository, in one
/// with no commits, or on a detached HEAD. Populated in #32.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitContext {
    /// Path to the repository root.
    pub repository: Option<PathBuf>,
    /// Branch name, absent on a detached HEAD.
    pub branch: Option<String>,
    /// Commit at the start of the session.
    pub commit_at_start: Option<String>,
    /// Commit at the end of the session.
    ///
    /// With `commit_at_start`, this is what makes "what changed during this
    /// session" answerable by diff rather than by guessing from the transcript.
    pub commit_at_end: Option<String>,
}

impl GitContext {
    /// Whether anything is actually recorded.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// One archived AI coding session.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Recall's identifier, derived from `provider` and `provider_session_id`.
    pub id: SessionId,
    /// The agent that produced the session.
    pub provider: Provider,
    /// The provider's own identifier, kept so a session can be traced back.
    pub provider_session_id: String,
    /// Model identifier, when the provider records one.
    pub model: Option<String>,
    /// When the session began.
    ///
    /// Required, not optional: the archive files sessions by this date, so a
    /// session without one cannot be stored. An adapter whose format has no
    /// explicit start must derive one from the session's own contents — the
    /// first event's timestamp, say. That is derivation, not invention.
    pub started_at: OffsetDateTime,
    /// When the session ended, if it has.
    pub ended_at: Option<OffsetDateTime>,
    /// The project the session was working on.
    pub project: Option<PathBuf>,
    /// Repository state, when the session ran inside one.
    pub git: Option<GitContext>,
}

impl Session {
    /// A session with only what is always known.
    ///
    /// The id is derived, so it is never passed in and cannot disagree with the
    /// provider fields it is built from.
    pub fn new(
        provider: Provider,
        provider_session_id: impl Into<String>,
        started_at: OffsetDateTime,
    ) -> Self {
        let provider_session_id = provider_session_id.into();
        Self {
            id: SessionId::derive(&provider, &provider_session_id),
            provider,
            provider_session_id,
            model: None,
            started_at,
            ended_at: None,
            project: None,
            git: None,
        }
    }

    /// Year, month and day the archive files this session under, in **UTC**.
    ///
    /// Local time would put the same session in different directories depending
    /// on where the machine was, which makes an archive non-reproducible. See
    /// `docs/archive-layout.md`.
    pub fn archive_date(&self) -> (i32, u8, u8) {
        let utc = self.started_at.to_offset(time::UtcOffset::UTC);
        (utc.year(), utc.month() as u8, utc.day())
    }

    /// How long the session lasted, when it has ended.
    pub fn duration(&self) -> Option<time::Duration> {
        self.ended_at.map(|end| end - self.started_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn provider(name: &str) -> Provider {
        Provider::new(name).expect("valid provider name")
    }

    #[test]
    fn ids_are_deterministic() {
        // This is what lets sync recognise a session it already archived.
        let a = SessionId::derive(&provider("claude-code"), "abc-123");
        let b = SessionId::derive(&provider("claude-code"), "abc-123");
        assert_eq!(a, b);
    }

    #[test]
    fn ids_are_filesystem_safe_on_every_platform() {
        // The whole reason the id is derived rather than reused: provider ids
        // may contain separators or characters Windows rejects.
        let hostile = "../../etc/passwd\\:*?\"<>|\0 and a very long tail";
        let id = SessionId::derive(&provider("claude-code"), hostile);

        assert_eq!(id.as_str().len(), ID_BYTES * 2);
        assert!(
            id.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "id was {:?}",
            id.as_str()
        );
    }

    #[test]
    fn the_same_provider_id_from_different_providers_is_a_different_session() {
        let a = SessionId::derive(&provider("claude-code"), "1");
        let b = SessionId::derive(&provider("codex"), "1");
        assert_ne!(a, b, "provider must be part of the identity");
    }

    #[test]
    fn provider_and_id_cannot_be_confused_for_one_another() {
        // Without a separator, ("ab", "c") and ("a", "bc") would hash the same.
        let a = SessionId::derive(&provider("ab"), "c");
        let b = SessionId::derive(&provider("a"), "bc");
        assert_ne!(a, b);
    }

    #[test]
    fn provider_names_are_restricted() {
        assert!(Provider::new("claude-code").is_ok());
        assert!(Provider::new("codex").is_ok());
        assert!(Provider::new("gemini-cli").is_ok());

        assert_eq!(Provider::new(""), Err(ProviderError::Empty));
        assert!(matches!(
            Provider::new("Claude"),
            Err(ProviderError::InvalidCharacter { character: 'C', .. })
        ));
        assert!(matches!(
            Provider::new("claude code"),
            Err(ProviderError::InvalidCharacter { character: ' ', .. })
        ));
        assert!(matches!(
            Provider::new("../etc"),
            Err(ProviderError::InvalidCharacter { .. })
        ));
        assert!(matches!(
            Provider::new("x".repeat(Provider::MAX_LEN + 1)),
            Err(ProviderError::TooLong { .. })
        ));
    }

    #[test]
    fn sessions_are_filed_by_utc_not_local_time() {
        // 01:30 on the 9th in a +05:00 zone is still the 8th in UTC. Filing by
        // local time would put this session in a different directory depending
        // on where the machine was.
        let s = Session::new(
            provider("claude-code"),
            "abc",
            datetime!(2026-09-09 01:30:00 +05:00),
        );
        assert_eq!(s.archive_date(), (2026, 9, 8));
    }

    #[test]
    fn a_session_starts_with_nothing_it_was_not_told() {
        let s = Session::new(
            provider("claude-code"),
            "abc",
            datetime!(2026-09-08 12:00:00 UTC),
        );
        assert_eq!(s.model, None);
        assert_eq!(s.ended_at, None);
        assert_eq!(s.project, None);
        assert_eq!(s.git, None);
        assert_eq!(s.duration(), None);
    }

    #[test]
    fn the_id_always_matches_the_provider_fields_it_came_from() {
        let s = Session::new(
            provider("claude-code"),
            "abc",
            datetime!(2026-09-08 12:00:00 UTC),
        );
        assert_eq!(s.id, SessionId::derive(&s.provider, &s.provider_session_id));
    }

    #[test]
    fn duration_is_measured_once_a_session_has_ended() {
        let mut s = Session::new(
            provider("claude-code"),
            "abc",
            datetime!(2026-09-08 12:00:00 UTC),
        );
        s.ended_at = Some(datetime!(2026-09-08 14:30:00 UTC));
        assert_eq!(s.duration(), Some(time::Duration::minutes(150)));
    }

    #[test]
    fn empty_git_context_is_recognisable() {
        assert!(GitContext::default().is_empty());
        assert!(!GitContext {
            branch: Some("dev".into()),
            ..Default::default()
        }
        .is_empty());
    }
}
