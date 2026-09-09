//! What Recall's exit codes mean, and the failures that produce them.
//!
//! A person reads the message; a script reads the code. Both need to be able to
//! tell "there is no such session" from "the archive is damaged", because the
//! first is a typo and the second is data loss.
//!
//! These are an interface. Adding a code is fine; changing what an existing one
//! means is a breaking change.

use std::path::PathBuf;

/// The command succeeded.
pub const SUCCESS: u8 = 0;
/// Something went wrong that has no more specific code.
pub const FAILURE: u8 = 1;
/// The command line could not be understood. Produced by the argument parser.
pub const USAGE: u8 = 2;
/// The command exists but does nothing yet.
pub const NOT_IMPLEMENTED: u8 = 3;
/// There is no `.recall/` here.
pub const NOT_INITIALIZED: u8 = 4;
/// No archived session matched.
pub const NOT_FOUND: u8 = 5;
/// More than one session matched, and guessing would be wrong.
pub const AMBIGUOUS: u8 = 6;
/// An archive exists but could not be read.
pub const DAMAGED: u8 = 7;
/// A file could not be read or written.
pub const IO: u8 = 8;

/// A failure Recall can describe precisely.
///
/// Typed rather than a string so the exit code comes from what went wrong
/// rather than from matching on a message that someone will reword later.
#[derive(Debug, thiserror::Error)]
pub enum Problem {
    #[error("Recall is not initialized in {} — run `recall init` first", .path.display())]
    NotInitialized { path: PathBuf },

    #[error("no archived session starts with {wanted:?} — try `recall sessions`")]
    NoSuchSession { wanted: String },

    #[error("{wanted:?} matches {count} sessions: {listed} — use more characters")]
    AmbiguousSession {
        wanted: String,
        count: usize,
        listed: String,
    },
}

impl Problem {
    /// The code this failure exits with.
    pub fn code(&self) -> u8 {
        match self {
            Problem::NotInitialized { .. } => NOT_INITIALIZED,
            Problem::NoSuchSession { .. } => NOT_FOUND,
            Problem::AmbiguousSession { .. } => AMBIGUOUS,
        }
    }
}

/// Work out which code a failure should exit with.
///
/// Walks the chain rather than looking only at the top, because the useful
/// classification is usually underneath a piece of context added on the way up.
pub fn code_for(error: &anyhow::Error) -> u8 {
    for cause in error.chain() {
        if let Some(problem) = cause.downcast_ref::<Problem>() {
            return problem.code();
        }
        if let Some(archive) = cause.downcast_ref::<recall_store::ArchiveError>() {
            return archive_code(archive);
        }
        if let Some(init) = cause.downcast_ref::<recall_store::InitError>() {
            return init_code(init);
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return IO;
        }
    }
    FAILURE
}

fn archive_code(error: &recall_store::ArchiveError) -> u8 {
    use recall_store::ArchiveError as E;
    match error {
        E::NotFound { .. } => NOT_FOUND,
        // A damaged archive is data loss, and a script watching for it should
        // not have to tell it apart from a typo.
        E::Corrupt { .. } | E::Decode { .. } | E::UnknownEncoding { .. } | E::NotAFile { .. } => {
            DAMAGED
        }
        E::Read { .. } | E::Write { .. } | E::Publish { .. } | E::CreateDirectory { .. } => IO,
        E::Encode { .. } | E::Search { .. } => FAILURE,
    }
}

fn init_code(error: &recall_store::InitError) -> u8 {
    use recall_store::InitError as E;
    match error {
        // An archive whose format this build cannot read is the same category
        // of problem as a damaged one: it is there, and it cannot be used.
        E::UnsupportedFormatVersion { .. }
        | E::MalformedConfig { .. }
        | E::ArchivedSessionsWithoutConfig { .. } => DAMAGED,
        E::NotADirectory { .. } => FAILURE,
        E::CreateDirectory { .. }
        | E::WriteFile { .. }
        | E::ReadDirectory { .. }
        | E::ReadConfig { .. } => IO,
    }
}
