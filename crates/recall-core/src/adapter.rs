//! The seam between a provider's session history and Recall's model.
//!
//! An adapter finds an AI coding agent's session files, parses them, and turns
//! them into [`Session`]s. Everything provider-specific lives behind this
//! trait; nothing above it knows which agent a session came from.
//!
//! Three rules, from `.github/CONTRIBUTING.md` and `.github/SECURITY.md`:
//!
//! - **Read-only.** An adapter never writes, moves, or deletes anything in a
//!   provider's directory.
//! - **Untrusted input.** Session files may contain anything the agent was
//!   shown. No value read from one ever becomes a path Recall opens.
//! - **No invention.** A field the provider does not record stays absent. An
//!   adapter derives (a start time from the first event, say) but never guesses.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::session::{Provider, Session};

/// Why an adapter could not do its job.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A provider file or directory could not be read.
    #[error("could not read {}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A session file exists but could not be understood.
    ///
    /// Scoped to one session on purpose: one unreadable file must never stop a
    /// sync from archiving the rest.
    #[error("{provider} session {provider_session_id} could not be parsed: {detail}")]
    Malformed {
        provider: Provider,
        provider_session_id: String,
        detail: String,
    },

    /// The session held nothing that could be turned into a session.
    #[error("{provider} session {provider_session_id} contains no usable records")]
    Empty {
        provider: Provider,
        provider_session_id: String,
    },
}

/// A session file an adapter found, before it has been read.
///
/// Discovery is deliberately cheap: it reports what exists without parsing it,
/// so a sync can decide what it already has before doing any real work. #25
/// depends on that split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSession {
    /// The provider's own identifier for this session.
    pub provider_session_id: String,
    /// Where the provider keeps it.
    ///
    /// Recall opens this read-only and never writes near it.
    pub path: PathBuf,
    /// The project the session appears to belong to, if the layout says.
    pub project: Option<PathBuf>,
    /// When the file was last written, if the filesystem could say.
    ///
    /// A hint for ordering work, not a session timestamp — the session's own
    /// records are the authority on when it happened.
    pub modified: Option<OffsetDateTime>,
}

/// One AI coding agent's session history.
pub trait Adapter {
    /// Which agent this adapter reads.
    fn provider(&self) -> &Provider;

    /// Where this agent keeps its sessions on this machine.
    ///
    /// Returned so a caller can report what was searched when nothing is found.
    fn search_roots(&self) -> Vec<PathBuf>;

    /// Find session files, without reading them.
    ///
    /// A missing directory means the agent is not installed, which is a normal
    /// condition and yields an empty list rather than an error.
    fn discover(&self) -> Result<Vec<DiscoveredSession>, AdapterError>;

    /// Read one discovered session into Recall's model.
    fn load(&self, discovered: &DiscoveredSession) -> Result<Session, AdapterError>;

    /// The provider version a session was written by, when it records one.
    ///
    /// Session formats move between releases, so this is worth capturing when
    /// diagnosing a parse failure.
    fn provider_version(&self, _path: &Path) -> Option<String> {
        None
    }
}
