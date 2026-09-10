//! `recall sessions` — what the archive holds.
//!
//! Answered from `.recall/index.db`, not by opening archives. Scanning the tree
//! meant decompressing the first line of every session on every listing, which
//! is the cost the index exists to remove.
//!
//! # When the index cannot answer
//!
//! The index is derived, so a missing or unusable one is never fatal — it is
//! rebuilt. The judgement is *when* to do that without turning a quick command
//! into a slow one behind the user's back.
//!
//! A rebuild happens only when the index cannot answer at all:
//!
//! - it was missing, unreadable, or written by a schema this build does not
//!   understand, so opening it created a new one; or
//! - it is empty while the archive is not, which is what a sync that never
//!   reached the index leaves behind.
//!
//! Otherwise the index is trusted, and the fast path never walks the archive.
//! `recall sync` is what keeps it current, and `--rebuild` forces the issue.
//! Checking the index against the tree on every listing would reintroduce the
//! traversal this command was built to avoid.

use std::path::Path;

use anyhow::Result;

use crate::exit::Problem;
use crate::indexing;
use recall_index::IndexedSession;
use recall_store::Archive;

/// One line's worth of a session.
struct Row {
    id: String,
    started: String,
    events: String,
    provider: String,
    model: String,
    branch: String,
}

/// List archived sessions, newest first.
pub fn list(project_root: &Path, force_rebuild: bool) -> Result<()> {
    let archive = Archive::open(project_root);
    if !archive.layout().root().is_dir() {
        return Err(Problem::NotInitialized {
            path: project_root.to_path_buf(),
        }
        .into());
    }

    let (mut index, recovered) = indexing::open(&archive)?;

    // The index could not answer, so it is rebuilt rather than reported as
    // broken. Saying so matters: this listing is slower than the others, and an
    // unexplained pause looks like a hang.
    let replaced = recovered != recall_index::Recovered::No;
    let empty_but_archives_exist = index.count()? == 0 && !archive.entries()?.is_empty();

    if force_rebuild || replaced || empty_but_archives_exist {
        if !force_rebuild {
            println!("The index is out of date — rebuilding it from the archives.");
        }
        let rebuilt = indexing::rebuild(&archive, &mut index)?;
        if force_rebuild {
            println!(
                "Rebuilt the index from {} archive{}.\n",
                rebuilt.indexed,
                if rebuilt.indexed == 1 { "" } else { "s" }
            );
        }
        if !rebuilt.unreadable.is_empty() {
            println!("{} could not be read:", rebuilt.unreadable.len());
            for reason in &rebuilt.unreadable {
                println!("  {reason}");
            }
            println!();
        }
    }

    let sessions = index.list()?;
    if sessions.is_empty() {
        println!("No sessions archived yet — run `recall sync`");
        return Ok(());
    }

    let rows: Vec<Row> = sessions.iter().map(row).collect();
    print_table(&rows);

    println!(
        "\n{} session{}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

fn row(indexed: &IndexedSession) -> Row {
    let s = &indexed.session;
    Row {
        // Enough to identify a session and to pass back to `recall show`.
        id: s.id.as_str()[..8].to_string(),
        started: s
            .started_at
            .format(&time::format_description::well_known::Rfc3339)
            .map(|t| t[..16].replace('T', " "))
            .unwrap_or_else(|_| "—".into()),
        // Archives written before the count was recorded do not carry one, and
        // guessing would mean decompressing the whole session.
        events: indexed
            .event_count
            .map_or_else(|| "?".to_string(), |n| n.to_string()),
        provider: s.provider.to_string(),
        model: s.model.clone().unwrap_or_else(|| "—".into()),
        branch: s
            .git
            .as_ref()
            .and_then(|g| g.branch.clone())
            .unwrap_or_else(|| "—".into()),
    }
}

/// Print aligned columns, sized to the content.
fn print_table(rows: &[Row]) {
    let width = |header: &str, f: fn(&Row) -> &str| {
        rows.iter()
            .map(|r| f(r).chars().count())
            .chain(std::iter::once(header.len()))
            .max()
            .unwrap_or(header.len())
    };

    let w_id = width("ID", |r| &r.id);
    let w_started = width("STARTED", |r| &r.started);
    let w_events = width("EVENTS", |r| &r.events);
    let w_provider = width("PROVIDER", |r| &r.provider);
    let w_model = width("MODEL", |r| &r.model);

    println!(
        "{id:<w_id$}  {started:<w_started$}  {events:>w_events$}  \
         {provider:<w_provider$}  {model:<w_model$}  BRANCH",
        id = "ID",
        started = "STARTED",
        events = "EVENTS",
        provider = "PROVIDER",
        model = "MODEL",
    );
    for r in rows {
        println!(
            "{:<w_id$}  {:<w_started$}  {:>w_events$}  {:<w_provider$}  {:<w_model$}  {}",
            r.id, r.started, r.events, r.provider, r.model, r.branch
        );
    }
}
