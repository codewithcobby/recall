//! The session archive.
//!
//! Writes normalized sessions into `.recall/sessions/` and reads them back.
//! The archive is the source of truth for conversation content, so writes are
//! atomic and a damaged archive is reported rather than silently repaired —
//! see `.github/SECURITY.md`.
//!
//! Implementation lands in #13.
