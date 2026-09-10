//! `recall verify` — check that the archives are still readable.
//!
//! `recall sessions` reads one line per archive, which is what makes it cheap
//! and what means it validates only that line. Corruption after the header is
//! invisible to it: a listing can show eleven healthy sessions when one of them
//! cannot be read.
//!
//! This is the expensive question asked deliberately. It decompresses every
//! archive in full, so Zstandard's frame checksums see every byte.

use std::path::Path;

use anyhow::Result;
use recall_store::Archive;

use crate::exit::Problem;

/// What a verification found.
#[derive(Debug, Default)]
pub struct Report {
    /// Sessions that read back completely.
    pub readable: usize,
    /// Sessions that did not, with the reason.
    pub damaged: Vec<Damaged>,
}

/// One archive that could not be read.
#[derive(Debug)]
pub struct Damaged {
    pub id: String,
    pub reason: String,
}

impl Report {
    /// How many archives were checked.
    pub fn checked(&self) -> usize {
        self.readable + self.damaged.len()
    }
}

/// Read every archive in full and report what survived.
pub fn verify(project_root: &Path, wanted: Option<&str>) -> Result<Report> {
    let archive = Archive::open(project_root);
    if !archive.layout().root().is_dir() {
        return Err(Problem::NotInitialized {
            path: project_root.to_path_buf(),
        }
        .into());
    }

    let ids = match wanted {
        Some(prefix) => {
            let matches = archive.resolve(prefix)?;
            match matches.len() {
                0 => {
                    return Err(Problem::NoSuchSession {
                        wanted: prefix.to_string(),
                    }
                    .into())
                }
                1 => matches,
                _ => {
                    let listed: Vec<_> = matches.iter().map(|id| &id.as_str()[..12]).collect();
                    return Err(Problem::AmbiguousSession {
                        wanted: prefix.to_string(),
                        count: matches.len(),
                        listed: listed.join(", "),
                    }
                    .into());
                }
            }
        }
        None => archive.entries()?.into_iter().map(|e| e.id).collect(),
    };

    let mut report = Report::default();
    for id in ids {
        // Every event, so the frame checksum covers every byte. Streaming keeps
        // that from costing memory proportional to the session.
        let outcome = archive.stream(&id).and_then(|stream| {
            for event in stream {
                event?;
            }
            Ok(())
        });

        match outcome {
            Ok(()) => report.readable += 1,
            Err(e) => report.damaged.push(Damaged {
                id: id.as_str()[..8].to_string(),
                // The cause is the useful half: "corrupt" says it is damaged,
                // the chain says how.
                reason: describe(&e),
            }),
        }
    }
    Ok(report)
}

/// An error and the reason underneath it.
fn describe(error: &recall_store::ArchiveError) -> String {
    let mut text = error.to_string();
    let mut cause = std::error::Error::source(error);
    while let Some(c) = cause {
        text.push_str(&format!(": {c}"));
        cause = c.source();
    }
    text
}
