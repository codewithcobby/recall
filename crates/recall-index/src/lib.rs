//! The metadata index.
//!
//! A SQLite database at `.recall/index.db` holding the metadata needed to find
//! a session without reading every archive.
//!
//! # The index is not the truth
//!
//! Everything in this crate is derived. The compressed archives under
//! `.recall/sessions/` are the source of truth for conversation content, and
//! every column here can be recomputed from them. Nothing that exists only in
//! the database is worth keeping.
//!
//! That single property is what makes the rest of the design simple:
//!
//! - **No conversation content is stored here.** Metadata and a pointer to the
//!   archive, nothing more. Deleting `index.db` loses no conversation.
//! - **A database that cannot be read is replaced, not repaired.** There is
//!   nothing in it to salvage, so recovery is a rebuild.
//! - **An unrecognised schema version is not a migration problem.** It can
//!   always be discarded and rebuilt.
//!
//! #37 states this boundary in the documentation and holds it with a test.
//!
//! # What this crate does not do
//!
//! It never reads an archive. Rebuilding means someone reads the archives and
//! hands the rows over — `recall-index` depends on `recall-core` alone, never
//! on `recall-store`, in line with the dependency direction in
//! `.github/CONTRIBUTING.md`.

pub mod index;
pub mod schema;
pub mod timestamp;

pub use index::{Index, IndexError, IndexedSession, Opened, Recovered};
pub use schema::SCHEMA_VERSION;
