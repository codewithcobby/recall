//! Recall's provider-independent core.
//!
//! This crate owns the domain: what a session is, what happens inside one, and
//! the traits that adapters, the archive, and the index implement. It knows
//! nothing about any particular AI coding agent, and it depends on none of the
//! crates that implement its traits — see the dependency direction in
//! `.github/CONTRIBUTING.md`.
//!
//! The types themselves land in #10 and #11.
