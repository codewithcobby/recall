//! The `recall` binary.
//!
//! Argument parsing, output, and exit codes only: this crate composes the
//! implementations and owns presentation, while the behaviour lives in the
//! crates it depends on.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod exit;
mod indexing;
mod sessions;
mod show;
mod sync;
mod verify;

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
                  machine.\n\nExit codes:\n  \
                  0  success\n  \
                  1  failed\n  \
                  2  the command line could not be understood\n  \
                  3  the command is not implemented yet\n  \
                  4  Recall is not initialized here\n  \
                  5  no such session\n  \
                  6  the session id was ambiguous\n  \
                  7  an archive is damaged or unreadable\n  \
                  8  a file could not be read or written",
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
        /// Print only the session's metadata, not its transcript.
        #[arg(long)]
        summary: bool,
    },
    /// Check that archived sessions are still readable.
    ///
    /// Reads every archive in full, unlike `recall sessions`, which reads only
    /// each one's first line.
    Verify {
        /// A session id or prefix. Omit to check every archive.
        session: Option<String>,
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
            Command::Sync => None,
            Command::Sessions => None,
            Command::Show { .. } => None,
            Command::Verify { .. } => None,
            Command::Search { .. } => Some(38),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Sync => "sync",
            Command::Sessions => "sessions",
            Command::Show { .. } => "show",
            Command::Verify { .. } => "verify",
            Command::Search { .. } => "search",
        }
    }
}

fn main() -> ExitCode {
    // Parsed rather than `Cli::parse()` so the usage exit code is ours and
    // documented, not whatever the argument parser happens to use. `--help` and
    // `--version` arrive here as errors too, and they are successes.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // `--help` and `--version` are requests and succeed. Help shown
            // *because* no command was given is a usage error: the user made a
            // mistake, and a script must be able to tell.
            let asked_for_it = matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            let _ = e.print();
            return ExitCode::from(if asked_for_it {
                exit::SUCCESS
            } else {
                exit::USAGE
            });
        }
    };

    if let Some(issue) = cli.command.tracking_issue() {
        eprintln!(
            "recall {}: not implemented yet (tracked in #{issue})",
            cli.command.name()
        );
        return ExitCode::from(exit::NOT_IMPLEMENTED);
    }

    let result = match cli.command {
        Command::Init => run_init(),
        Command::Sync => run_sync(),
        Command::Sessions => run_sessions(),
        Command::Show { session, summary } => run_show(&session, summary),
        Command::Verify { session } => run_verify(session.as_deref()),
        // Every other command returned above.
        _ => unreachable!("handled by the tracking-issue branch"),
    };

    match result {
        Ok(()) => ExitCode::from(exit::SUCCESS),
        Err(e) => {
            // The chain matters: the top line says what failed, the causes say
            // why. The code says which kind of failure it was, so a script can
            // tell a typo from data loss without parsing English.
            eprintln!("error: {e:#}");
            ExitCode::from(exit::code_for(&e))
        }
    }
}

/// `recall sessions`
fn run_sessions() -> Result<()> {
    let project_root =
        std::env::current_dir().context("could not determine the current directory")?;
    sessions::list(&project_root)
}

/// `recall show`
fn run_show(session: &str, summary: bool) -> Result<()> {
    let project_root =
        std::env::current_dir().context("could not determine the current directory")?;
    show::show(&project_root, session, summary)
}

/// `recall verify`
fn run_verify(session: Option<&str>) -> Result<()> {
    let project_root =
        std::env::current_dir().context("could not determine the current directory")?;
    let report = verify::verify(&project_root, session)?;

    if report.checked() == 0 {
        println!("No sessions archived yet — run `recall sync`");
        return Ok(());
    }

    println!(
        "{} session{} checked: {} readable, {} damaged",
        report.checked(),
        if report.checked() == 1 { "" } else { "s" },
        report.readable,
        report.damaged.len()
    );

    if report.damaged.is_empty() {
        return Ok(());
    }

    println!();
    for damaged in &report.damaged {
        println!("  {}  {}", damaged.id, damaged.reason);
    }

    // A damaged archive is data loss, and a script should be able to notice
    // without reading English.
    Err(exit::Problem::ArchivesDamaged {
        count: report.damaged.len(),
    }
    .into())
}

/// `recall sync`
fn run_sync() -> Result<()> {
    let project_root =
        std::env::current_dir().context("could not determine the current directory")?;
    let summary = sync::sync(&project_root)?;

    if summary.found == 0 {
        println!("No AI sessions found for {}", project_root.display());
        return Ok(());
    }

    println!(
        "{} session{} found: {} archived, {} already had",
        summary.found,
        if summary.found == 1 { "" } else { "s" },
        summary.archived,
        summary.already_had
    );

    if summary.in_progress > 0 {
        println!(
            "  {} still being written — left for a later run, so nothing is archived half-finished",
            summary.in_progress
        );
    }

    if summary.had_failures() {
        println!("\n{} could not be archived:", summary.failures.len());
        for failure in &summary.failures {
            println!(
                "  {} {}: {}",
                failure.provider, failure.provider_session_id, failure.reason
            );
        }
    }

    // Deliberately not an error. Everything reported above is in the archive;
    // what is missing is the shortcut for finding it again, and the next sync
    // notices and repairs it.
    if let Some(problem) = &summary.index_problem {
        println!("\nThe index could not be brought up to date: {problem}");
        println!("  Archived sessions are unaffected. The next sync will retry.");
    }
    Ok(())
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
