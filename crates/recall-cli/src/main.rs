//! The `recall` binary.
//!
//! Argument parsing, output, and exit codes only: this crate composes the
//! implementations and owns presentation, while the behaviour lives in the
//! crates it depends on.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use recall_store::InitOutcome;

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

/// The commands Recall exposes.
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
    /// The issue that implements this command, for the ones that do not work yet.
    fn tracking_issue(&self) -> Option<u32> {
        match self {
            Command::Init => None,
            Command::Sync => Some(24),
            Command::Sessions => Some(28),
            Command::Show { .. } => Some(29),
            Command::Search { .. } => Some(38),
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

/// A command exists but does nothing yet.
const EXIT_NOT_IMPLEMENTED: u8 = 3;
/// The command failed.
const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(issue) = cli.command.tracking_issue() {
        eprintln!(
            "recall {}: not implemented yet (tracked in #{issue})",
            cli.command.name()
        );
        return ExitCode::from(EXIT_NOT_IMPLEMENTED);
    }

    let result = match cli.command {
        Command::Init => run_init(),
        // Every other command returned above.
        _ => unreachable!("handled by the tracking-issue branch"),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The chain matters: the top line says what failed, the causes say why.
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// `recall init`
fn run_init() -> Result<()> {
    let project_root =
        std::env::current_dir().context("could not determine the current directory")?;

    let outcome = recall_store::init(&project_root)
        .with_context(|| format!("could not initialize Recall in {}", project_root.display()))?;

    report(&outcome, &project_root);
    Ok(())
}

/// Say what happened, and what the user may want to do next.
fn report(outcome: &InitOutcome, project_root: &Path) {
    let shown = |p: &PathBuf| -> String {
        p.strip_prefix(project_root)
            .unwrap_or(p)
            .display()
            .to_string()
    };

    // Creating the archive directory itself means this was a fresh start.
    // Creating anything else means an earlier run was interrupted and this one
    // finished the job — which is not the same thing, and should not claim to be.
    let started_fresh = outcome.created.contains(&outcome.root);

    if outcome.created_anything() {
        if started_fresh {
            println!("Initialized Recall in {}", outcome.root.display());
        } else {
            println!("Completed Recall setup in {}", outcome.root.display());
        }
        for path in &outcome.created {
            println!("  created {}", shown(path));
        }
    } else {
        println!(
            "Recall is already initialized in {}",
            outcome.root.display()
        );
    }

    if !outcome.unrecognized.is_empty() {
        println!(
            "\nLeft alone ({} not owned by Recall):",
            if outcome.unrecognized.len() == 1 {
                "1 entry"
            } else {
                "entries"
            }
        );
        for name in &outcome.unrecognized {
            println!("  {name}");
        }
    }

    // Only on a run that changed something. Repeating it on every subsequent
    // `recall init` is noise, and the suggestion has already been made.
    if outcome.created_anything() {
        if let Some(hint) = gitignore_hint(project_root) {
            println!("\n{hint}");
        }
    }
}

/// Suggest ignoring the archive, without touching the user's `.gitignore`.
///
/// Whether session history belongs in a project's git history is the user's
/// call, so this only ever prints.
fn gitignore_hint(project_root: &Path) -> Option<String> {
    let gitignore = project_root.join(".gitignore");
    let contents = std::fs::read_to_string(&gitignore).ok()?;
    if contents.lines().any(|l| l.trim().starts_with(".recall")) {
        return None;
    }
    Some(
        "Your .gitignore does not mention .recall/. Add it if the archive should stay \
         local, or commit it deliberately to make session history part of the project."
            .to_string(),
    )
}
