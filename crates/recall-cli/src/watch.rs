//! `recall watch` — archive sessions as they happen, without being asked.
//!
//! `recall sync` only helps someone who remembers to run it, and the sessions
//! worth keeping are exactly the ones nobody thinks to capture at the time.
//!
//! Two things make this more than a loop around `sync`.
//!
//! **Filesystem events are noisy.** An agent appending to a transcript
//! produces a burst of them, and some platforms report the same change more
//! than once. Acting on each one would run a sync per keystroke, so events are
//! debounced: a pass runs once the writing has stopped for
//! [`DEBOUNCE`].
//!
//! **An event means a session is being written, not that it is finished.**
//! `sync` deliberately leaves a file alone until it has been quiet for
//! [`recall sync`'s quiet period][super::sync::QUIET_PERIOD], because archives
//! are never rewritten and capturing a conversation mid-flight would freeze its
//! first half and lose the rest. So the pass triggered by an event almost
//! always archives nothing — the session is still going. Something has to come
//! back later, once the file has settled, or the session that just ended would
//! sit unarchived until the user happened to type something else. That is what
//! [`SWEEP`] is for.
//!
//! **Events cannot be relied on at all.** On macOS, FSEvents does not report an
//! append until the writing process closes the file — measured, not assumed:
//! a test that appended to an open handle saw no event whatsoever until the
//! handle was dropped. An agent that holds its transcript open for the length
//! of a conversation therefore produces nothing to react to.
//!
//! So [`SWEEP`] is not a safety net for the timing above; it is the mechanism
//! that makes watch mode work on such a platform, and events are what make it
//! feel immediate when they happen to arrive. Watch mode would still archive
//! every session with the event stream removed entirely, just less promptly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::out::outln;
use crate::sync;

/// How long the provider directories must be quiet before a pass runs.
///
/// Long enough that a burst of writes becomes one pass, short enough that it
/// still feels immediate.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// How often to run a pass even when nothing has changed.
///
/// The reason is [`sync::QUIET_PERIOD`]: a session that has just ended is not
/// archivable yet, and the last write to it is the last event there will ever
/// be. Without a sweep, the pass that would finally archive it never runs.
///
/// Cheap enough to do on a timer — a sync with nothing new to do recognises
/// every archived session from its id without reading any of them.
pub const SWEEP: Duration = Duration::from_secs(30);

/// How long to block waiting for an event before looking at the clock.
///
/// Only affects how promptly an interrupt and an elapsed timer are noticed.
const POLL: Duration = Duration::from_millis(200);

/// Watch the provider directories and archive sessions as they settle.
///
/// Returns when interrupted. Blocks until then.
pub fn watch(project_root: &Path) -> Result<()> {
    // Up front, and before any watcher is set up: this fails when the project
    // is not initialized, and it archives anything that settled while nothing
    // was watching. A watcher that started by silently ignoring the backlog
    // would be a poor first impression.
    let summary = sync::sync(project_root)?;
    sync::report(project_root, &summary);

    let roots = watch_roots();
    if roots.is_empty() {
        // No provider directories exist, so nothing will ever produce an event.
        // Watching would be a process that cannot do anything.
        outln!("No AI coding agents found on this machine — nothing to watch.");
        return Ok(());
    }

    for root in &roots {
        outln!("Watching {}", root.display());
    }
    outln!("Press Ctrl-C to stop.");

    let stop = interrupt_flag()?;
    run_loop(&roots, &stop, || {
        // A whole `sync`, every pass, rather than ingesting just what changed.
        //
        // That is the point: watch has no ingestion path of its own to drift
        // from this one, and #25's deduplication means a pass with nothing new
        // recognises every archived session from its id without reading any of
        // them.
        //
        // It also re-opens the index each time, which looks wasteful and is
        // deliberate. Holding the database open for the life of the watcher
        // would lock it against every other `recall` command the user runs
        // while watching.
        match sync::sync(project_root) {
            // Silence when a pass changes nothing. Most do: a session is
            // usually still being written when its events arrive.
            Ok(summary) if is_quiet(&summary) => {}
            Ok(summary) => sync::report(project_root, &summary),
            // A failed pass must not end the watch. Whatever broke may be one
            // unreadable session, and the next pass tries again.
            Err(e) => outln!("Could not archive: {e:#}"),
        }
    })?;

    outln!("Stopped watching.");
    Ok(())
}

/// Watch `roots` and call `pass` whenever the work should be done.
///
/// Separated from [`watch`] so the timing rules can be tested against a
/// temporary directory: the real thing watches the agents' own directories,
/// and no test may write into those.
///
/// Returns when `stop` becomes true, or when the watcher goes away.
fn run_loop(roots: &[PathBuf], stop: &AtomicBool, mut pass: impl FnMut()) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        // A send failure means the receiver is gone, which happens as the
        // command shuts down. Not worth reporting.
        let _ = tx.send(event);
    })
    .context("could not start watching the filesystem")?;

    for root in roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("could not watch {}", root.display()))?;
    }

    let mut pending: Option<Instant> = None;
    let mut last_pass = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(POLL) {
            Ok(Ok(event)) if is_interesting(&event.kind) => {
                // Restarted on every event, so a burst of writes becomes one
                // pass once the writing stops rather than one pass per write.
                pending = Some(Instant::now());
            }
            Ok(Ok(_)) => {}
            // One unreadable event is not a reason to stop watching; the sweep
            // will pick up whatever it described.
            Ok(Err(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The watcher is gone, so no further event will arrive.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        let settled = pending.is_some_and(|at| at.elapsed() >= DEBOUNCE);
        if !settled && last_pass.elapsed() < SWEEP {
            continue;
        }

        pending = None;
        last_pass = Instant::now();
        pass();
    }

    Ok(())
}

/// Whether a pass did anything worth telling the user about.
fn is_quiet(summary: &sync::Summary) -> bool {
    summary.archived == 0 && !summary.had_failures() && summary.index_problem.is_none()
}

/// Whether an event might mean a session changed.
///
/// Reads and metadata touches are not changes to a transcript, and on some
/// platforms they arrive constantly. Anything not understood counts as a
/// change: a pass costs little, and a missed session costs the conversation.
fn is_interesting(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any => true,
        EventKind::Access(_) | EventKind::Other => false,
    }
}

/// The provider directories to watch.
///
/// Taken from the adapters themselves rather than listed here, so an adapter
/// added later is watched without this file changing. A second list would be a
/// second thing to remember, and the symptom of forgetting it — one provider
/// silently never watched — is invisible until someone loses a conversation.
///
/// A directory that does not exist is skipped: that agent is not installed, and
/// watching a path that is not there is not something every platform supports.
fn watch_roots() -> Vec<PathBuf> {
    sync::adapters()
        .iter()
        .flat_map(|adapter| adapter.search_roots())
        .filter(|root| root.is_dir())
        .collect()
}

/// A flag that becomes true when the user interrupts.
///
/// Ctrl-C otherwise kills the process where it stands. Nothing is corrupted by
/// that — archives are written to a temporary file and renamed into place — but
/// the command should stop because it was asked to, not because it was killed.
fn interrupt_flag() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let handler = Arc::clone(&stop);
    ctrlc::set_handler(move || handler.store(true, Ordering::Relaxed))
        .context("could not listen for Ctrl-C")?;
    Ok(stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

    #[test]
    fn writes_and_deletions_are_worth_a_pass() {
        assert!(is_interesting(&EventKind::Create(CreateKind::File)));
        assert!(is_interesting(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_interesting(&EventKind::Remove(RemoveKind::File)));
    }

    #[test]
    fn an_unrecognised_event_is_treated_as_a_change() {
        // Cheaper to run a pass that finds nothing than to miss a session.
        assert!(is_interesting(&EventKind::Any));
    }

    #[test]
    fn merely_reading_a_file_is_not_a_change() {
        // These arrive constantly on some platforms, and Recall itself
        // generates them by reading the very files it is watching.
        assert!(!is_interesting(&EventKind::Access(AccessKind::Read)));
        assert!(!is_interesting(&EventKind::Access(AccessKind::Open(
            notify::event::AccessMode::Read
        ))));
        assert!(!is_interesting(&EventKind::Other));
    }

    #[test]
    fn a_pass_that_changed_nothing_says_nothing() {
        let quiet = sync::Summary {
            found: 3,
            already_had: 3,
            ..sync::Summary::default()
        };
        assert!(is_quiet(&quiet));
    }

    #[test]
    fn a_pass_that_archived_something_is_reported() {
        let busy = sync::Summary {
            found: 1,
            archived: 1,
            ..sync::Summary::default()
        };
        assert!(!is_quiet(&busy));
    }

    #[test]
    fn a_failure_is_reported_even_when_nothing_was_archived() {
        let failed = sync::Summary {
            found: 1,
            failures: vec![sync::Failure {
                provider: "claude-code".into(),
                provider_session_id: "s".into(),
                reason: "unreadable".into(),
            }],
            ..sync::Summary::default()
        };
        assert!(!is_quiet(&failed));
    }

    #[test]
    fn an_index_problem_is_reported_even_when_nothing_was_archived() {
        let problem = sync::Summary {
            found: 1,
            already_had: 1,
            index_problem: Some("database is locked".into()),
            ..sync::Summary::default()
        };
        assert!(!is_quiet(&problem));
    }

    #[test]
    fn a_session_still_being_written_is_not_worth_announcing() {
        // The ordinary case while someone is working: the event fired, the
        // file is still growing, and the sweep will archive it once it stops.
        let in_progress = sync::Summary {
            found: 1,
            in_progress: 1,
            ..sync::Summary::default()
        };
        assert!(is_quiet(&in_progress));
    }

    #[test]
    fn every_adapter_is_watched_without_watch_keeping_its_own_list() {
        // A hand-maintained list here would go stale the first time an adapter
        // was added, and the symptom — one provider silently never watched —
        // does not show up until someone loses a conversation.
        let from_adapters: Vec<PathBuf> = sync::adapters()
            .iter()
            .flat_map(|adapter| adapter.search_roots())
            .filter(|root| root.is_dir())
            .collect();
        assert_eq!(watch_roots(), from_adapters);
    }

    #[test]
    fn the_sweep_is_shorter_than_the_quiet_period() {
        // Otherwise a session that just became archivable would wait for the
        // sweep after the one that should have caught it.
        assert!(
            SWEEP < sync::QUIET_PERIOD,
            "a settled session would not be archived promptly"
        );
    }

    #[test]
    fn the_debounce_is_short_enough_to_feel_immediate() {
        assert!(DEBOUNCE < Duration::from_secs(2));
        // And longer than the poll, or a burst would be split across passes.
        assert!(DEBOUNCE > POLL);
    }

    /// How long a test will wait for the watcher to report. Generous, because
    /// it is a failure deadline rather than a synchronisation device: the test
    /// blocks on a channel and proceeds the moment the pass happens.
    const DEADLINE: Duration = Duration::from_secs(20);

    /// How long to wait before concluding that no further pass is coming.
    ///
    /// Only used to assert an absence, which cannot be observed any other way.
    /// Comfortably longer than a debounce and shorter than a sweep.
    const SETTLE: Duration = Duration::from_secs(3);

    /// The real loop, running over a temporary directory.
    ///
    /// The command watches the agents' own directories and no test may write
    /// into those, so the loop is driven here against a directory of the
    /// test's own.
    ///
    /// Nothing here sleeps to synchronise: every wait blocks on the channel
    /// the loop reports passes through, and returns the moment one arrives.
    struct Watching {
        dir: tempfile::TempDir,
        stop: Arc<AtomicBool>,
        passes: mpsc::Receiver<()>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Watching {
        /// Start watching, and return only once the watcher is demonstrably
        /// live.
        ///
        /// Establishing a watch is not instant on any platform. A test that
        /// wrote its file first would lose the event and fail for a reason
        /// that has nothing to do with the code. So this writes throwaway
        /// probes until one is actually reported, which proves events are
        /// flowing before the test does anything it intends to measure.
        fn start() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let root = dir.path().to_path_buf();
            let stop = Arc::new(AtomicBool::new(false));
            let (tx, passes) = mpsc::channel();

            let loop_stop = Arc::clone(&stop);
            let loop_root = root.clone();
            let worker = std::thread::spawn(move || {
                run_loop(&[loop_root], &loop_stop, || {
                    let _ = tx.send(());
                })
                .expect("watch loop");
            });

            let watching = Self {
                dir,
                stop,
                passes,
                worker: Some(worker),
            };

            let probe = watching.path(".probe");
            let deadline = Instant::now() + DEADLINE;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "the watcher never reported a change"
                );
                std::fs::write(&probe, b"probe").expect("write probe");
                if watching.passes.recv_timeout(DEBOUNCE * 4).is_ok() {
                    break;
                }
            }

            // Anything the probes left in flight belongs to establishing the
            // watch, not to the test. Wait for the loop to fall quiet so a
            // later assertion cannot be answered by a stale event.
            watching.settle();
            watching
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        /// Block until the loop runs a pass. False if it never does.
        fn saw_a_pass(&self) -> bool {
            self.passes.recv_timeout(DEADLINE).is_ok()
        }

        /// Drain passes until none arrives for [`SETTLE`].
        fn settle(&self) {
            while self.passes.recv_timeout(SETTLE).is_ok() {}
        }

        /// Whether exactly one pass happened, with nothing following it.
        fn saw_exactly_one_pass(&self) -> bool {
            if !self.saw_a_pass() {
                return false;
            }
            self.passes.recv_timeout(SETTLE).is_err()
        }
    }

    impl Drop for Watching {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    #[test]
    fn a_new_session_file_triggers_a_pass() {
        // The whole point of the command: something appears, and Recall acts
        // without being asked.
        let w = Watching::start();
        std::fs::write(w.path("session.jsonl"), b"{}\n").expect("write session");
        assert!(w.saw_a_pass(), "a new file did not trigger a pass");
    }

    #[test]
    fn a_file_that_grows_triggers_a_pass() {
        // A session being appended to, which is what a live transcript is.
        //
        // The handle is closed before the assertion on purpose. On macOS,
        // FSEvents reports nothing for an append until the writer closes the
        // file — with the handle still open this test saw no event at all, and
        // waited out its whole deadline. That is a real property of the
        // platform rather than a quirk of the test, and it is why watch mode
        // cannot depend on events alone. See SWEEP.
        let w = Watching::start();
        let path = w.path("session.jsonl");
        std::fs::write(&path, b"{}\n").expect("write");
        assert!(w.saw_a_pass(), "creating the file did not trigger a pass");
        w.settle();

        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        f.write_all(b"{\"more\":true}\n").expect("append");
        f.sync_all().expect("sync");
        drop(f);

        assert!(w.saw_a_pass(), "appending to a file did not trigger a pass");
    }

    #[test]
    fn a_burst_of_writes_becomes_one_pass() {
        // An agent appending to a transcript produces a burst of events, and
        // some platforms report the same change more than once. A pass per
        // event would run a sync per keystroke.
        let w = Watching::start();
        let path = w.path("session.jsonl");
        for i in 0..20 {
            std::fs::write(&path, format!("{{\"n\":{i}}}\n")).expect("write");
        }
        assert!(
            w.saw_exactly_one_pass(),
            "a burst of writes was not debounced into a single pass"
        );
    }

    #[test]
    fn a_file_written_in_chunks_is_one_pass_not_one_per_chunk() {
        let w = Watching::start();
        let path = w.path("session.jsonl");
        use std::io::Write as _;
        let mut f = std::fs::File::create(&path).expect("create");
        for i in 0..10 {
            f.write_all(format!("{{\"chunk\":{i}}}\n").as_bytes())
                .expect("write chunk");
            f.flush().expect("flush");
        }
        assert!(
            w.saw_exactly_one_pass(),
            "chunked writing produced more than one pass"
        );
    }

    #[test]
    fn a_removed_file_triggers_a_pass() {
        // Removal matters: what the archive knew about that file may need
        // reconciling, and the pass is what notices.
        let w = Watching::start();
        let path = w.path("session.jsonl");
        std::fs::write(&path, b"{}\n").expect("write");
        assert!(w.saw_a_pass(), "creating the file did not trigger a pass");
        w.settle();

        std::fs::remove_file(&path).expect("remove");
        assert!(w.saw_a_pass(), "a removal did not trigger a pass");
    }

    #[test]
    fn the_loop_stops_when_it_is_told_to() {
        // What Ctrl-C relies on: the flag is the only thing that ends the loop,
        // and it ends it without needing an event to arrive first.
        let w = Watching::start();
        w.stop.store(true, Ordering::Relaxed);

        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            if w.worker.as_ref().is_some_and(|h| h.is_finished()) {
                return;
            }
            std::thread::yield_now();
        }
        panic!("the loop did not stop when the flag was set");
    }
}
