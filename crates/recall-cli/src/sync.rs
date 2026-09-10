//! `recall sync` — discover provider sessions and archive them.
//!
//! The first command that does what Recall exists for: everything before this
//! built one half or the other, and this is where they meet.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::exit::Problem;
use recall_adapters::ClaudeCode;
use recall_core::{Adapter, AdapterError, DiscoveredSession, GitContext, Session, SessionId};
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
    /// Sessions still being written, left for a later run.
    pub in_progress: usize,
    /// Sessions that could not be archived, with the reason.
    ///
    /// Collected rather than returned early: one unreadable session must not
    /// cost the user every session after it.
    pub failures: Vec<Failure>,
}

/// How long a session file must be untouched before it is archived.
///
/// Archives are never rewritten, and an already-archived session is skipped
/// without being read. Archiving a conversation that is still going would
/// therefore capture its first half and lose the rest permanently — the worst
/// possible outcome for a tool whose purpose is preservation.
///
/// Waiting costs only latency: the session is archived by the next run, and
/// `recall watch` (#48) will make that automatic.
pub const QUIET_PERIOD: Duration = Duration::from_secs(5 * 60);

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
    if !archive.layout().root().is_dir() {
        return Err(Problem::NotInitialized {
            path: project_root.to_path_buf(),
        }
        .into());
    }

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

        // A session still being appended to is left for a later run.
        if let Some(modified) = session.modified {
            if is_recent(modified, QUIET_PERIOD) {
                summary.in_progress += 1;
                continue;
            }
        }

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
            Ok(Outcome::Stored(Stored::Written(_))) => summary.archived += 1,
            Ok(Outcome::Stored(Stored::AlreadyPresent(_))) => summary.already_had += 1,
            Ok(Outcome::StillBeingWritten) => summary.in_progress += 1,
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
) -> Result<Outcome, String> {
    let before = written_at(&discovered.path);
    let mut session = adapter.load(discovered).map_err(describe)?;

    // The session may have grown while it was being read, in which case what
    // was loaded is a snapshot of something still in motion. Archiving it would
    // freeze that snapshot forever.
    if written_at(&discovered.path) != before {
        return Ok(Outcome::StillBeingWritten);
    }

    add_repository(&mut session);

    archive
        .write(&session)
        .map(Outcome::Stored)
        .map_err(|e| describe_archive(&e))
}

/// Record which repository the session's project is in.
///
/// The provider is the authority on everything it recorded. Claude Code writes
/// the branch that was checked out *during* the session; detection here runs
/// afterwards, potentially days later and on a different branch entirely. So
/// what the adapter supplied is never overwritten — this only fills what it
/// could not answer.
///
/// In practice that means the repository root, which no provider records, and
/// the branch when the provider had none.
fn add_repository(session: &mut Session) {
    let Some(project) = session.project.as_deref() else {
        return;
    };
    let Some(detected) = recall_git::detect(project) else {
        // Not in a repository, or no git. Ordinary.
        return;
    };

    let existing = session.git.take().unwrap_or_default();
    session.git = Some(GitContext {
        repository: existing.repository.or(detected.repository),
        // What was true during the session beats what is true now.
        branch: existing.branch.or(detected.branch),
        // Neither the provider nor detection can say which commit a session
        // started or ended on. See recall_git's `commits_are_not_guessed`.
        commit_at_start: existing.commit_at_start,
        commit_at_end: existing.commit_at_end,
    });
}

/// What happened to one session.
enum Outcome {
    Stored(Stored),
    /// It changed while it was being read.
    StillBeingWritten,
}

/// When a file was last written, if the filesystem will say.
fn written_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Whether `moment` is within `window` of now.
///
/// A clock that has moved backwards makes this answer "no", which errs towards
/// archiving. That is the right way to be wrong: the alternative is refusing to
/// archive anything until the clock is fixed.
fn is_recent(moment: time::OffsetDateTime, window: Duration) -> bool {
    let now = time::OffsetDateTime::now_utc();
    match now - moment {
        elapsed if elapsed.is_negative() => false,
        elapsed => elapsed.unsigned_abs() < window,
    }
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
    fn a_file_written_moments_ago_is_treated_as_in_progress() {
        let now = time::OffsetDateTime::now_utc();
        assert!(is_recent(now, QUIET_PERIOD));
        assert!(is_recent(now - time::Duration::minutes(1), QUIET_PERIOD));
    }

    #[test]
    fn a_file_untouched_for_longer_than_the_quiet_period_is_settled() {
        let now = time::OffsetDateTime::now_utc();
        assert!(!is_recent(now - time::Duration::minutes(10), QUIET_PERIOD));
        assert!(!is_recent(now - time::Duration::days(30), QUIET_PERIOD));
    }

    #[test]
    fn a_timestamp_in_the_future_does_not_block_archiving() {
        // A clock that has moved, or a file copied from a machine whose clock
        // differs. Refusing to archive until the clock is fixed would be the
        // worse failure.
        let ahead = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
        assert!(!is_recent(ahead, QUIET_PERIOD));
    }

    /// A session carrying whatever git metadata a provider supplied.
    fn session_with(project: Option<&str>, git: Option<GitContext>) -> Session {
        let mut s = Session::new(
            recall_core::Provider::new("claude-code").expect("provider"),
            "s",
            time::OffsetDateTime::now_utc(),
        );
        s.project = project.map(PathBuf::from);
        s.git = git;
        s
    }

    #[test]
    fn what_the_provider_recorded_is_never_overwritten() {
        // Claude Code writes the branch that was checked out *during* the
        // session. Detection runs afterwards, possibly days later and on a
        // different branch. History wins.
        let mut session = session_with(
            Some("/definitely/not/a/repository"),
            Some(GitContext {
                branch: Some("security/143-refresh-token-rotation".into()),
                ..GitContext::default()
            }),
        );
        add_repository(&mut session);

        assert_eq!(
            session.git.and_then(|g| g.branch).as_deref(),
            Some("security/143-refresh-token-rotation")
        );
    }

    #[test]
    fn a_session_outside_a_repository_gains_nothing() {
        let mut session = session_with(Some("/definitely/not/a/repository"), None);
        add_repository(&mut session);
        // Nothing detected, so nothing invented.
        assert!(session.git.is_none() || session.git.as_ref().is_some_and(GitContext::is_empty));
    }

    #[test]
    fn a_session_with_no_project_gains_nothing() {
        let mut session = session_with(None, None);
        add_repository(&mut session);
        assert!(session.git.is_none());
    }

    #[test]
    fn commits_are_still_not_filled_in() {
        // Neither the provider nor detection can say which commit a session
        // started or ended on. #71 works out a deterministic relationship.
        let mut session = session_with(Some("."), None);
        add_repository(&mut session);
        if let Some(git) = session.git {
            assert_eq!(git.commit_at_start, None);
            assert_eq!(git.commit_at_end, None);
        }
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
