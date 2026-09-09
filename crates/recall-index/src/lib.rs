//! The metadata index.
//!
//! A SQLite database at `.recall/index.db` holding the metadata needed to find
//! a session without reading every archive. Everything here is derived data and
//! can be rebuilt from the archives.
//!
//! Implementation lands in #34.
