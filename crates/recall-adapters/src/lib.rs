//! Provider adapters.
//!
//! One module per AI coding agent, each responsible for finding that agent's
//! session files, parsing them, and normalizing the result into
//! [`recall_core`]'s model. Adapters open provider files read-only and treat
//! their contents as untrusted input.
//!
//! Provider-specific types stay inside this crate. The Claude Code adapter
//! lands in #19.
