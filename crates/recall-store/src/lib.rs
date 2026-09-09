//! The session archive.
//!
//! Writes normalized sessions into `.recall/sessions/` and reads them back.
//! The archive is the source of truth for conversation content, so writes are
//! atomic and a damaged archive is reported rather than silently repaired —
//! see `.github/SECURITY.md`.
//!
//! The on-disk layout is specified in `docs/archive-layout.md`.
//!
//! Reading and writing sessions lands in #13.

pub mod archive;
pub mod init;
pub mod layout;

pub use archive::{Archive, ArchiveEntry, ArchiveError, Stored};
pub use init::{init, InitError, InitOutcome};
pub use layout::{Layout, FORMAT_VERSION};
