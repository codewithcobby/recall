//! What repository a session ran against.
//!
//! Read-only and deterministic. Recall asks git four questions and records the
//! answers; it never runs a command that changes anything, and every question
//! has "nothing to say" as a valid answer.
//!
//! # Why shell out
//!
//! Recall runs `git` rather than linking a git library. Both are defensible and
//! #31 required the choice to be recorded, because everything later builds on
//! it:
//!
//! - It adds no dependency, and no C toolchain to build.
//! - It answers with whatever the user's own git would answer, including their
//!   configuration, their worktree layout, and their version's behaviour. A
//!   library can disagree with the git the user actually has.
//! - Recall is a developer tool. Git is present.
//!
//! The cost is that git might be missing or broken. That is treated the same as
//! every other unanswerable question: absence, not failure. A sync must not stop
//! because a project is not in a repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use recall_core::GitContext;

/// The git command Recall runs.
const GIT: &str = "git";

/// Look up the repository state at `path`.
///
/// Returns `None` when there is nothing to report — not a repository, git is
/// unavailable, or the directory is gone. Never an error: a session outside a
/// repository is ordinary, and archiving it must not fail.
pub fn detect(path: &Path) -> Option<GitContext> {
    let repository = repository_root(path)?;

    let context = GitContext {
        repository: Some(repository),
        branch: branch(path),
        // The commits a session started and ended on are deliberately not
        // filled in here. See `commits_are_not_guessed` below.
        commit_at_start: None,
        commit_at_end: None,
    };

    Some(context)
}

/// The root of the repository containing `path`.
///
/// Uses git's own answer rather than walking upwards looking for `.git`, which
/// would get worktrees and `GIT_DIR` wrong.
pub fn repository_root(path: &Path) -> Option<PathBuf> {
    run(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// The branch checked out at `path`.
///
/// `None` on a detached HEAD, which is not a branch — the same decision #122
/// made for the branch Claude Code records. Every detached session recorded as
/// being on a branch called `HEAD` collapses into one meaningless group.
///
/// An unborn branch — a repository with no commits yet — still has a name, and
/// that name is returned.
pub fn branch(path: &Path) -> Option<String> {
    run(path, &["symbolic-ref", "--quiet", "--short", "HEAD"])
}

/// The commit currently checked out at `path`.
///
/// `None` in a repository with no commits.
pub fn head_commit(path: &Path) -> Option<String> {
    run(path, &["rev-parse", "HEAD"])
}

/// Run a read-only git command and return its trimmed output.
///
/// Every argument is a fixed string chosen here. Nothing read out of a session
/// ever reaches this — see `.github/SECURITY.md`.
fn run(directory: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(GIT)
        // -C rather than changing the process's directory, which would be a
        // global mutation in a program that may be doing other things.
        .arg("-C")
        .arg(directory)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a git command for test setup, failing loudly if it does not work.
    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new(GIT)
            .arg("-C")
            .arg(dir)
            // Identity fixed so the tests do not depend on whoever runs them
            // or on their git configuration. These are git-level options and
            // must come before the subcommand.
            .args(["-c", "user.email=test@example.invalid"])
            .args(["-c", "user.name=Test"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// A repository built here, so no test depends on the repository it runs in.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
        dir
    }

    fn commit(dir: &Path, message: &str) {
        git(dir, &["commit", "--quiet", "--allow-empty", "-m", message]);
    }

    #[test]
    fn a_repository_reports_its_root_branch_and_nothing_invented() {
        let r = repo();
        commit(r.path(), "first");

        let context = detect(r.path()).expect("a repository");
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert!(context.repository.is_some());

        // The root git reports may differ from the temp path by a symlink —
        // /var against /private/var on macOS — so compare what both resolve to.
        let reported = context.repository.expect("a root").canonicalize().ok();
        assert_eq!(reported, r.path().canonicalize().ok());
    }

    #[test]
    fn a_directory_that_is_not_a_repository_reports_nothing() {
        // Ordinary, not a failure: plenty of projects are not in git, and
        // archiving one must not stop because of it.
        let plain = tempfile::tempdir().expect("temp dir");
        assert!(detect(plain.path()).is_none());
        assert!(repository_root(plain.path()).is_none());
        assert!(branch(plain.path()).is_none());
        assert!(head_commit(plain.path()).is_none());
    }

    #[test]
    fn a_repository_with_no_commits_still_has_a_branch() {
        // An unborn branch has a name even though nothing points at it.
        let r = repo();
        let context = detect(r.path()).expect("a repository");
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert_eq!(head_commit(r.path()), None, "a commit was invented");
    }

    #[test]
    fn a_detached_head_is_not_a_branch() {
        // The same decision #122 made for the branch Claude Code records:
        // recording "HEAD" collapses every detached session everywhere into one
        // meaningless group.
        let r = repo();
        commit(r.path(), "first");
        git(r.path(), &["checkout", "--quiet", "--detach"]);

        let context = detect(r.path()).expect("a repository");
        assert_eq!(context.branch, None);
        // The commit is still there, and is what identifies such a session.
        assert!(head_commit(r.path()).is_some());
    }

    #[test]
    fn a_subdirectory_reports_the_repository_it_belongs_to() {
        let r = repo();
        commit(r.path(), "first");
        let nested = r.path().join("src/deep");
        std::fs::create_dir_all(&nested).expect("mkdir");

        let context = detect(&nested).expect("a repository");
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert_eq!(
            context.repository.expect("a root").canonicalize().ok(),
            r.path().canonicalize().ok(),
            "a subdirectory reported itself rather than the repository"
        );
    }

    #[test]
    fn a_worktree_reports_itself_not_the_checkout_it_came_from() {
        // Worktrees are how several of the sessions in a real archive were
        // produced, so this is not a hypothetical.
        let r = repo();
        commit(r.path(), "first");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let tree = elsewhere.path().join("wt");
        git(
            r.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "side",
                tree.to_str().expect("utf8 path"),
            ],
        );

        let context = detect(&tree).expect("a repository");
        assert_eq!(context.branch.as_deref(), Some("side"));
        assert_eq!(
            context.repository.expect("a root").canonicalize().ok(),
            tree.canonicalize().ok(),
            "the worktree reported the checkout it was created from"
        );
    }

    #[test]
    fn a_branch_name_with_slashes_survives() {
        let r = repo();
        commit(r.path(), "first");
        git(
            r.path(),
            &["checkout", "--quiet", "-b", "fix/312-vendor-order"],
        );

        assert_eq!(
            branch(r.path()).as_deref(),
            Some("fix/312-vendor-order"),
            "a slash in a branch name was mangled"
        );
    }

    #[test]
    fn commits_are_not_guessed() {
        // Claude Code records the branch but not the commit, and a sync runs
        // after a session ends — often long after, and possibly on a different
        // branch entirely. There is no deterministic way to say which commit a
        // session started or ended on, so both stay absent. Filling them with
        // whatever HEAD happens to be at sync time would be invention, and
        // #71 is where a deterministic relationship gets worked out.
        let r = repo();
        commit(r.path(), "first");

        let context = detect(r.path()).expect("a repository");
        assert_eq!(context.commit_at_start, None);
        assert_eq!(context.commit_at_end, None);
    }

    #[test]
    fn detection_never_writes_to_the_repository() {
        let r = repo();
        commit(r.path(), "first");
        let before = head_commit(r.path());
        let status_before = Command::new(GIT)
            .arg("-C")
            .arg(r.path())
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");

        for _ in 0..3 {
            detect(r.path());
        }

        assert_eq!(head_commit(r.path()), before, "the commit moved");
        let status_after = Command::new(GIT)
            .arg("-C")
            .arg(r.path())
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");
        assert_eq!(
            status_before.stdout, status_after.stdout,
            "detection changed the working tree"
        );
    }
}
