//! `recall sessions` — what the archive holds.
//!
//! The first command that reads anything back. Until now Recall could preserve
//! a conversation and offer no way to see that it had.

use std::path::Path;

use anyhow::Result;

use crate::exit::Problem;
use recall_core::SessionHeader;
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
pub fn list(project_root: &Path) -> Result<()> {
    let archive = Archive::open(project_root);
    if !archive.layout().root().is_dir() {
        return Err(Problem::NotInitialized {
            path: project_root.to_path_buf(),
        }
        .into());
    }

    let results = archive.headers()?;
    if results.is_empty() {
        println!("No sessions archived yet — run `recall sync`");
        return Ok(());
    }

    let mut headers: Vec<SessionHeader> = Vec::new();
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(header) => headers.push(header),
            Err(e) => failures.push(e),
        }
    }

    // Newest first: the session someone wants is almost always a recent one.
    headers.sort_by_key(|h| std::cmp::Reverse(h.session.started_at));

    let rows: Vec<Row> = headers.iter().map(row).collect();
    print_table(&rows);

    println!(
        "\n{} session{}",
        headers.len(),
        if headers.len() == 1 { "" } else { "s" }
    );

    if !failures.is_empty() {
        // Reported rather than hidden: an unreadable archive is exactly what
        // someone listing their sessions needs to know about.
        println!("\n{} could not be read:", failures.len());
        for failure in &failures {
            println!("  {failure}");
        }
    }
    Ok(())
}

fn row(header: &SessionHeader) -> Row {
    let s = &header.session;
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
        events: header
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
