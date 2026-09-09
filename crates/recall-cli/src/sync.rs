//! `recall sync` — discover provider sessions and archive them.
//!
//! The first command that does what Recall exists for: everything before this
//! built one half or the other, and this is where they meet.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use recall_adapters::ClaudeCode;
use recall_core::{Adapter, AdapterError, DiscoveredSession, SessionId};
use recall_store::{Archive, Stored};

/// What one sync run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Sessions the adapters found for this project.
    pub found: usize,
    /// Sessions archived by this run.
    pub archived: usize,
    /// Sessions already in the archive.
    ///
    /// Recognised without reading them.
    pub already_had: usize,
    /// Sessions that could not be archived, with the reason.
    ///
    /// Collected rather than returned early: one unreadable session must not
    /// cost the user every session after it.
    pub failures: Vec<Failure>,
}

/// One session that could not be archived.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub provider: String,
    pub provider_session_id: String,
    pub reason: String,
}

impl Summary {
    /// Whether anything went wrong.
    pub fn had_failures(&self) -> bool {
        !self.failures.is_empty()
    }
}

/// Archive every session belonging to this project.
pub fn sync(project_root: &Path) -> Result<Summary> {
    let archive = Archive::open(project_root);
    anyhow::ensure!(
        archive.layout().root().is_dir(),
        "Recall is not initialized in {} — run `recall init` first",
        project_root.display()
    );

    let mut summary = Summary::default();
    for adapter in adapters() {
        sync_adapter(adapter.as_ref(), project_root, &archive, &mut summary)?;
    }
    Ok(summary)
}

/// Every adapter Recall knows about.
///
/// Codex is #40, Gemini #44.
fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(ClaudeCode::new())]
}

/// Archive one adapter's sessions.
fn sync_adapter(
    adapter: &dyn Adapter,
    project_root: &Path,
    archive: &Archive,
    summary: &mut Summary,
) -> Result<()> {
    let discovered = adapter
        .discover()
        .with_context(|| format!("could not look for {} sessions", adapter.provider()))?;

    for session in discovered {
        if !belongs_to(&session, project_root) {
            continue;
        }
        summary.found += 1;

        // Recall's session id derives from the provider and the provider's own
        // id, so it is knowable before anything is read. Checking here is what
        // makes a repeat sync cheap: the alternative is parsing a transcript to
        // discover it was already archived. This is what the discover/load
        // split in the adapter trait was for.
        let id = SessionId::derive(adapter.provider(), &session.provider_session_id);
        match archive.locate(&id) {
            Ok(Some(_)) => {
                summary.already_had += 1;
                continue;
            }
            Ok(None) => {}
            // A broken archive is not a reason to skip the session, but it is
            // worth reporting rather than silently re-archiving over it.
            Err(e) => {
                summary.failures.push(Failure {
                    provider: adapter.provider().to_string(),
                    provider_session_id: session.provider_session_id.clone(),
                    reason: e.to_string(),
                });
                continue;
            }
        }

        match archive_one(adapter, archive, &session) {
            Ok(Stored::Written(_)) => summary.archived += 1,
            Ok(Stored::AlreadyPresent(_)) => summary.already_had += 1,
            Err(reason) => summary.failures.push(Failure {
                provider: adapter.provider().to_string(),
                provider_session_id: session.provider_session_id.clone(),
                reason,
            }),
        }
    }
    Ok(())
}

/// Load one session and put it in the archive.
///
/// Returns the reason as text rather than an error type: a failure here is
/// reported and the run continues, so it is data rather than control flow.
fn archive_one(
    adapter: &dyn Adapter,
    archive: &Archive,
    discovered: &DiscoveredSession,
) -> Result<Stored, String> {
    let session = adapter.load(discovered).map_err(describe)?;
    archive.write(&session).map_err(|e| describe_archive(&e))
}

/// Whether a session was worked on inside this project.
///
/// Recall's archive lives in a project, so a sync archives that project's
/// sessions. A session from a subdirectory counts — work in `src/` is work on
/// the project — but one from an unrelated directory does not.
fn belongs_to(session: &DiscoveredSession, project_root: &Path) -> bool {
    let Some(project) = session.project.as_deref() else {
        // Nothing says where it ran, so nothing says it belongs here.
        return false;
    };
    let project = canonical(project);
    let root = canonical(project_root);
    project.starts_with(&root)
}

/// Resolve a path as far as the filesystem allows.
///
/// A provider's recorded path may name a directory that no longer exists, so
/// failure to canonicalize is normal and the path is used as given.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn describe(error: AdapterError) -> String {
    match error {
        AdapterError::Empty { .. } => "no usable records in the session file".to_string(),
        other => other.to_string(),
    }
}

fn describe_archive(error: &recall_store::ArchiveError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovered(project: Option<&str>) -> DiscoveredSession {
        DiscoveredSession {
            provider_session_id: "s".into(),
            path: PathBuf::from("/somewhere/s.jsonl"),
            additional_paths: Vec::new(),
            project: project.map(PathBuf::from),
            modified: None,
        }
    }

    #[test]
    fn a_session_from_this_project_belongs_here() {
        assert!(belongs_to(
            &discovered(Some("/w/demo")),
            Path::new("/w/demo")
        ));
    }

    #[test]
    fn a_session_from_a_subdirectory_belongs_here() {
        // Work in src/ is work on the project.
        assert!(belongs_to(
            &discovered(Some("/w/demo/src")),
            Path::new("/w/demo")
        ));
    }

    #[test]
    fn a_session_from_elsewhere_does_not() {
        assert!(!belongs_to(
            &discovered(Some("/w/other")),
            Path::new("/w/demo")
        ));
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_does_not_belong() {
        // /w/demo-old is not inside /w/demo, however similar the strings look.
        assert!(!belongs_to(
            &discovered(Some("/w/demo-old")),
            Path::new("/w/demo")
        ));
    }

    #[test]
    fn a_session_with_no_recorded_project_does_not_belong() {
        // Nothing says where it ran, so nothing says it belongs here. Archiving
        // it would put another project's conversation in this project's
        // archive.
        assert!(!belongs_to(&discovered(None), Path::new("/w/demo")));
    }

    #[test]
    fn syncing_without_init_says_so() {
        let project = tempfile::tempdir().expect("temp dir");
        let err = sync(project.path()).expect_err("must refuse");
        assert!(
            err.to_string().contains("recall init"),
            "the error did not say what to do: {err}"
        );
    }

    #[test]
    fn a_summary_reports_its_failures() {
        let mut summary = Summary {
            found: 2,
            archived: 1,
            ..Summary::default()
        };
        assert!(!summary.had_failures());
        summary.failures.push(Failure {
            provider: "claude-code".into(),
            provider_session_id: "s".into(),
            reason: "unreadable".into(),
        });
        assert!(summary.had_failures());
    }
}
