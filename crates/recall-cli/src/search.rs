//! `recall search` — find the conversation where something was worked out.
//!
//! # Where the text comes from
//!
//! The archives, read at query time. Not the index.
//!
//! This is the decision #38 asked to be made explicitly, because the obvious
//! implementation — a full-text index in `index.db` — would put conversation
//! content in the database and contradict rule 1 of
//! `docs/index-and-archive.md`. Measured on real transcripts, it would also
//! make the database **6.5x the size of the archive it indexes**, and 12.5x on
//! a larger one. The "small derived cache" would become the largest thing in
//! `.recall/`.
//!
//! Scanning costs time instead of space:
//!
//! | archive | full scan |
//! |---|---|
//! | 8 real sessions, 5.4 MB, 22.6 MB of text | ~50 ms |
//! | 2000 sessions, 16 MB, 139 MB of text | ~2 s |
//!
//! For the archives people actually have, that is indistinguishable from
//! instant, and the boundary survives untouched. A full-text index can be added
//! later as an opt-in accelerator without changing what a search *means* —
//! which is the part that would be hard to take back.
//!
//! A consequence worth knowing: search needs no index at all. Deleting
//! `index.db` does not affect it.

use std::path::Path;

use anyhow::Result;

use crate::exit::Problem;
use crate::out::outln;
use recall_core::{Query, SessionHeader};
use recall_store::Archive;

/// How much text to show around a hit.
const SNIPPET: usize = 110;

/// How many hits to show per session before summarising the rest.
///
/// A session can match hundreds of times, and printing all of them buries the
/// other sessions that matched.
const SHOWN_PER_SESSION: usize = 3;

/// One matching event.
struct Hit {
    kind: &'static str,
    snippet: String,
}

/// One session that matched, and what matched in it.
struct Match {
    header: SessionHeader,
    hits: Vec<Hit>,
    total: usize,
}

/// Search archived sessions.
pub fn search(project_root: &Path, query: &str) -> Result<()> {
    let archive = Archive::open(project_root);
    if !archive.layout().root().is_dir() {
        return Err(Problem::NotInitialized {
            path: project_root.to_path_buf(),
        }
        .into());
    }

    let query = Query::parse(query).map_err(|_| Problem::EmptyQuery)?;

    let mut matches = Vec::new();
    let mut unreadable = Vec::new();

    for entry in archive.entries()? {
        // Streamed, not loaded: a session can be tens of thousands of events,
        // and only the matching ones are kept.
        let mut stream = match archive.stream(&entry.id) {
            Ok(stream) => stream,
            Err(e) => {
                unreadable.push(format!("{}  {e}", &entry.id.as_str()[..8]));
                continue;
            }
        };
        let header = stream.header().clone();

        let mut hits = Vec::new();
        let mut total = 0;
        let mut damaged = None;

        for event in &mut stream {
            let event = match event {
                Ok(event) => event,
                // Stop at the damage but keep what was found before it, then
                // say so. Silently reporting a partial result as a complete one
                // is the failure mode worth avoiding here.
                Err(e) => {
                    damaged = Some(e.to_string());
                    break;
                }
            };
            let Some(text) = event.searchable_text() else {
                continue;
            };
            let Some(at) = query.first_match(text) else {
                continue;
            };

            total += 1;
            if hits.len() < SHOWN_PER_SESSION {
                hits.push(Hit {
                    kind: event.kind(),
                    snippet: recall_core::snippet(text, at, SNIPPET),
                });
            }
        }

        if let Some(reason) = damaged {
            unreadable.push(format!("{}  {reason}", &entry.id.as_str()[..8]));
        }
        if total > 0 {
            matches.push(Match {
                header,
                hits,
                total,
            });
        }
    }

    // Newest first, like the listing: the session someone wants is usually
    // recent.
    matches.sort_by_key(|m| std::cmp::Reverse(m.header.session.started_at));

    report(&query, &matches, &unreadable);
    Ok(())
}

fn report(query: &Query, matches: &[Match], unreadable: &[String]) {
    if matches.is_empty() {
        outln!("No session matches {query}");
    } else {
        let sessions = matches.len();
        let hits: usize = matches.iter().map(|m| m.total).sum();
        outln!(
            "{sessions} session{} matching {query} ({hits} match{})",
            if sessions == 1 { "" } else { "s" },
            if hits == 1 { "" } else { "es" }
        );
        outln!();

        for m in matches {
            let s = &m.header.session;
            let started = s
                .started_at
                .format(&time::format_description::well_known::Rfc3339)
                .map(|t| t[..16].replace('T', " "))
                .unwrap_or_else(|_| "—".into());
            let branch = s
                .git
                .as_ref()
                .and_then(|g| g.branch.as_deref())
                .unwrap_or("—");

            outln!("{}  {started}  {branch}", &s.id.as_str()[..8]);
            for hit in &m.hits {
                outln!("    {:<10} {}", hit.kind, hit.snippet);
            }
            if m.total > m.hits.len() {
                let more = m.total - m.hits.len();
                outln!(
                    "    + {more} more match{}",
                    if more == 1 { "" } else { "es" }
                );
            }
            outln!();
        }

        // The id is what `recall show` takes, and someone who has just found a
        // session almost always wants to read it.
        outln!("Read one with `recall show <id>`");
    }

    if !unreadable.is_empty() {
        outln!();
        outln!(
            "{} archive{} could not be searched:",
            unreadable.len(),
            if unreadable.len() == 1 { "" } else { "s" }
        );
        for reason in unreadable {
            outln!("  {reason}");
        }
        outln!("Run `recall verify` for the full picture.");
    }
}
