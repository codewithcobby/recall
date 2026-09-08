//! The `recall` binary.
//!
//! Argument parsing, output, and exit codes only: this crate composes the
//! implementations and owns presentation, while the behaviour lives in the
//! crates it depends on.

use clap::{Parser, Subcommand};

/// Local memory for AI coding sessions.
#[derive(Parser)]
#[command(
    name = "recall",
    version,
    about = "Local memory for AI coding sessions",
    long_about = "Recall discovers and archives AI coding sessions locally, so the work \
                  done in a session outlives the session itself.\n\nNothing leaves your \
                  machine.",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The commands Recall will expose.
///
/// Every variant is unimplemented at this point; each is filled in by the issue
/// named against it, and until then reports that rather than pretending to work.
#[derive(Subcommand)]
enum Command {
    /// Initialize Recall for this project.
    Init,
    /// Discover and archive new AI sessions.
    Sync,
    /// List archived sessions.
    Sessions,
    /// Show an archived session.
    Show {
        /// Session id, or an unambiguous prefix of one.
        session: String,
    },
    /// Search archived sessions.
    Search {
        /// What to look for.
        query: String,
    },
}

impl Command {
    /// The issue that implements this command.
    fn tracking_issue(&self) -> u32 {
        match self {
            Command::Init => 7,
            Command::Sync => 24,
            Command::Sessions => 28,
            Command::Show { .. } => 29,
            Command::Search { .. } => 38,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Sync => "sync",
            Command::Sessions => "sessions",
            Command::Show { .. } => "show",
            Command::Search { .. } => "search",
        }
    }
}

/// Exit code for a command that exists but does nothing yet.
const EXIT_NOT_IMPLEMENTED: i32 = 3;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    eprintln!(
        "recall {}: not implemented yet (tracked in #{})",
        cli.command.name(),
        cli.command.tracking_issue()
    );
    std::process::ExitCode::from(EXIT_NOT_IMPLEMENTED as u8)
}
