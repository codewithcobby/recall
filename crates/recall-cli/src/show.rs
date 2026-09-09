//! `recall show` — read one archived session back.

use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;

use crate::exit::Problem;
use recall_core::{Session, SessionEvent};
use recall_store::Archive;

/// Print an archived session.
pub fn show(project_root: &Path, wanted: &str, metadata_only: bool) -> Result<()> {
    let archive = Archive::open(project_root);
    if !archive.layout().root().is_dir() {
        return Err(Problem::NotInitialized {
            path: project_root.to_path_buf(),
        }
        .into());
    }

    let matches = archive.resolve(wanted)?;
    let id = match matches.len() {
        0 => {
            return Err(Problem::NoSuchSession {
                wanted: wanted.to_string(),
            }
            .into())
        }
        1 => matches.into_iter().next().expect("exactly one"),
        // Showing one of several would be showing the wrong conversation some
        // of the time, which is worse than asking.
        _ => {
            let listed: Vec<_> = matches.iter().map(|id| &id.as_str()[..12]).collect();
            return Err(Problem::AmbiguousSession {
                wanted: wanted.to_string(),
                count: matches.len(),
                listed: listed.join(", "),
            }
            .into());
        }
    };

    let session = archive.read(&id)?;

    // Locked and buffered: a large transcript is thousands of writes, and
    // unbuffered stdout makes that painfully slow.
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    write_metadata(&mut out, &session)?;
    if !metadata_only {
        writeln!(out)?;
        for event in &session.events {
            write_event(&mut out, event)?;
        }
    }
    out.flush()?;
    Ok(())
}

fn write_metadata(out: &mut impl Write, s: &Session) -> Result<()> {
    writeln!(out, "session  {}", s.id)?;
    writeln!(out, "provider {}", s.provider)?;
    if let Some(model) = &s.model {
        writeln!(out, "model    {model}")?;
    }
    writeln!(out, "started  {}", s.started_at)?;
    if let Some(ended) = s.ended_at {
        writeln!(out, "ended    {ended}")?;
    }
    if let Some(minutes) = s.duration().map(|d| d.whole_minutes()) {
        writeln!(out, "lasted   {minutes} minutes")?;
    }
    if let Some(project) = &s.project {
        writeln!(out, "project  {}", project.display())?;
    }
    if let Some(branch) = s.git.as_ref().and_then(|g| g.branch.as_deref()) {
        writeln!(out, "branch   {branch}")?;
    }
    writeln!(out, "events   {}", s.event_count())?;
    Ok(())
}

/// One event, labelled by who or what produced it.
///
/// Tool calls, results, commands and file changes are rendered rather than
/// summarised — being able to see what the agent actually did is the reason the
/// archive keeps them.
fn write_event(out: &mut impl Write, event: &SessionEvent) -> Result<()> {
    let at = event
        .at()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map(|t| t[11..19].to_string())
        .unwrap_or_else(|| "        ".into());

    match event {
        SessionEvent::UserMessage { content, .. } => {
            writeln!(out, "{at}  you")?;
            write_block(out, content)?;
        }
        SessionEvent::AssistantMessage { content, .. } => {
            writeln!(out, "{at}  assistant")?;
            write_block(out, content)?;
        }
        SessionEvent::ToolCall {
            name, arguments, ..
        } => {
            writeln!(out, "{at}  → {name}")?;
            if let Some(args) = arguments {
                write_block(out, args)?;
            }
        }
        SessionEvent::ToolResult {
            content, failed, ..
        } => {
            let marker = if *failed == Some(true) {
                " (failed)"
            } else {
                ""
            };
            writeln!(out, "{at}  ← result{marker}")?;
            write_block(out, content)?;
        }
        SessionEvent::Command {
            command,
            exit_code,
            output,
            ..
        } => {
            let code = exit_code.map_or(String::new(), |c| format!(" (exit {c})"));
            writeln!(out, "{at}  $ {command}{code}")?;
            if let Some(output) = output {
                write_block(out, output)?;
            }
        }
        SessionEvent::FileChange { action, path, .. } => {
            writeln!(out, "{at}  {} {}", action.as_str(), path.display())?;
        }
        SessionEvent::Other {
            provider_kind,
            content,
            ..
        } => {
            writeln!(out, "{at}  [{provider_kind}]")?;
            write_block(out, content)?;
        }
    }
    Ok(())
}

/// Indent a block of text under its heading.
///
/// Not truncated. A transcript that quietly drops the long tool result is not a
/// transcript, and #29 exists so someone can read what actually happened.
fn write_block(out: &mut impl Write, text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    for line in text.lines() {
        writeln!(out, "    {line}")?;
    }
    Ok(())
}
