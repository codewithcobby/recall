//! `recall search`, end to end.
//!
//! The query goes straight from the command line into matching, so the cases
//! that matter most are the hostile ones: empty queries, regex and SQL
//! metacharacters, and a damaged archive. None of them may crash, and none may
//! return results that are quietly wrong.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn recall_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("failed to run recall")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn make_settled(path: &Path) {
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open for touch")
        .set_modified(long_ago)
        .expect("backdate");
}

/// A session whose transcript says exactly `text`.
fn claude_session(home: &Path, project: &Path, id: &str, day: u8, text: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let body = format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-09-{day:02}T12:00:00.000Z\",\"cwd\":\"{p}\",\"gitBranch\":\"dev\",\"message\":{{\"role\":\"user\",\"content\":\"{escaped}\"}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":\"2026-09-{day:02}T12:00:00.000Z\",\"cwd\":\"{p}\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[{{\"type\":\"text\",\"text\":\"understood\"}}]}}}}\n",
        p = project.display()
    );
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(&path, body).expect("write session");
    make_settled(&path);
}

/// A project holding one session per line of `texts`.
fn archived(texts: &[&str]) -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");

    assert!(recall_in(&root, home.path(), &["init"]).status.success());
    for (n, text) in texts.iter().enumerate() {
        claude_session(
            home.path(),
            &root,
            &format!("session-{n}"),
            (n + 1) as u8,
            text,
        );
    }
    let sync = recall_in(&root, home.path(), &["sync"]);
    assert!(sync.status.success(), "sync failed: {sync:?}");
    (home, project, root)
}

fn archives(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.join(".recall/sessions")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                found.push(p);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_matching_query_names_the_session_and_shows_the_text() {
    let (home, _p, root) = archived(&[
        "we rewrote the payment orchestrator today",
        "unrelated work on the login form",
    ]);

    let out = recall_in(&root, home.path(), &["search", "payment orchestrator"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);

    assert!(text.contains("1 session matching"), "{text}");
    assert!(text.contains("payment orchestrator"), "{text}");
    // The id is what `recall show` takes, so it has to be there.
    assert!(text.contains("recall show"), "{text}");
}

#[test]
fn a_query_that_matches_nothing_says_so_and_succeeds() {
    // Finding nothing is an answer, not a failure.
    let (home, _p, root) = archived(&["we rewrote the payment orchestrator"]);

    let out = recall_in(&root, home.path(), &["search", "kubernetes"]);
    assert!(out.status.success(), "finding nothing must not be an error");
    assert!(
        stdout(&out).contains("No session matches"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_query_matching_many_sessions_reports_them_all() {
    let (home, _p, root) = archived(&[
        "the parser was slow",
        "the parser needed a rewrite",
        "the parser is fine now",
        "nothing to do with it",
    ]);

    let text = stdout(&recall_in(&root, home.path(), &["search", "parser"]));
    assert!(text.contains("3 sessions matching"), "{text}");
}

#[test]
fn results_are_newest_first() {
    let (home, _p, root) = archived(&["parser one", "parser two", "parser three"]);

    let text = stdout(&recall_in(&root, home.path(), &["search", "parser"]));
    let order: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("2026-09-"))
        .map(|l| l.split_whitespace().nth(1).unwrap_or(""))
        .collect();
    let mut expected = order.clone();
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(order, expected, "not newest first:\n{text}");
}

#[test]
fn an_empty_query_is_refused_rather_than_matching_everything() {
    let (home, _p, root) = archived(&["anything at all"]);

    for empty in ["", "   "] {
        let out = recall_in(&root, home.path(), &["search", empty]);
        assert!(!out.status.success(), "{empty:?} was accepted");
        // Exit 2 is "the command line could not be understood".
        assert_eq!(out.status.code(), Some(2), "{empty:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("nothing to search for"), "{stderr}");
    }
}

#[test]
fn metacharacters_are_searched_for_literally() {
    // The injection and pattern cases #39 asks to be deliberate about. Each is
    // stored in a session and then searched for verbatim: the query language
    // has no operators, so each must find itself and nothing else.
    let hostile = [
        "100% of the budget",
        "a _private field",
        "glob a*b matching",
        "it's a quote",
        "; DROP TABLE sessions; --",
        "regex .* everything",
        "a (group|alternation)",
        "back\\slash",
        "[square brackets]",
    ];
    let (home, _p, root) = archived(&hostile);

    for phrase in hostile {
        let out = recall_in(&root, home.path(), &["search", phrase]);
        assert!(out.status.success(), "{phrase:?} failed: {out:?}");
        let text = stdout(&out);
        assert!(
            text.contains("session matching"),
            "{phrase:?} did not find itself:\n{text}"
        );
    }
}

#[test]
fn a_sql_injection_attempt_is_text_and_the_archive_survives() {
    let (home, _p, root) = archived(&["ordinary content"]);
    let before = archives(&root).len();

    let out = recall_in(
        &root,
        home.path(),
        &["search", "'; DROP TABLE sessions; SELECT '"],
    );
    assert!(out.status.success());
    assert!(stdout(&out).contains("No session matches"));

    // Nothing was executed, so nothing was destroyed.
    assert_eq!(archives(&root).len(), before);
    assert!(recall_in(&root, home.path(), &["sessions"])
        .status
        .success());
}

#[test]
fn a_wildcard_does_not_match_more_than_itself() {
    let (home, _p, root) = archived(&["the payment service", "a pay*ment literal"]);

    let text = stdout(&recall_in(&root, home.path(), &["search", "pay*ment"]));
    assert!(text.contains("1 session matching"), "{text}");
    assert!(text.contains("literal"), "{text}");
}

#[test]
fn several_words_do_not_have_to_be_quoted() {
    // `recall search retry backoff` is what people type. Requiring quotes made
    // it fail with "unexpected argument", which was found by running the
    // documented walkthrough rather than by a test.
    let (home, _p, root) = archived(&["retry with exponential backoff", "unrelated work"]);

    let unquoted = recall_in(&root, home.path(), &["search", "retry", "backoff"]);
    assert!(unquoted.status.success(), "{unquoted:?}");
    assert!(stdout(&unquoted).contains("1 session matching"));

    // And it means the same as quoting the whole thing.
    let quoted = stdout(&recall_in(&root, home.path(), &["search", "retry backoff"]));
    assert_eq!(stdout(&unquoted), quoted);
}

#[test]
fn search_with_no_query_at_all_is_a_usage_error() {
    let (home, _p, root) = archived(&["anything"]);
    let out = recall_in(&root, home.path(), &["search"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn a_quoted_phrase_matches_only_the_phrase() {
    let (home, _p, root) = archived(&[
        "the payment orchestrator retries",
        "the orchestrator handles payment",
    ]);

    let text = stdout(&recall_in(
        &root,
        home.path(),
        &["search", "\"payment orchestrator\""],
    ));
    assert!(text.contains("1 session matching"), "{text}");
}

#[test]
fn a_damaged_archive_is_reported_and_the_rest_are_still_searched() {
    // A corrupt archive must not hide the sessions that are fine, and must not
    // be passed over in silence either.
    let (home, _p, root) = archived(&["parser one", "parser two", "parser three"]);
    let victim = archives(&root).remove(1);
    fs::write(&victim, b"no longer an archive").expect("damage");

    let out = recall_in(&root, home.path(), &["search", "parser"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);

    assert!(text.contains("2 sessions matching"), "{text}");
    assert!(text.contains("could not be searched"), "{text}");
    assert!(text.contains("recall verify"), "{text}");
}

#[test]
fn a_truncated_archive_does_not_produce_silent_results() {
    // Truncation is the nastier case: the header reads fine and the transcript
    // stops partway, so a naive reader reports a partial session as a whole one.
    let (home, _p, root) = archived(&["parser one", "parser two"]);
    let victim = archives(&root).remove(0);
    let bytes = fs::read(&victim).expect("read");
    fs::write(&victim, &bytes[..bytes.len() / 2]).expect("truncate");

    let out = recall_in(&root, home.path(), &["search", "parser"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        stdout(&out).contains("could not be searched"),
        "truncation was not reported:\n{}",
        stdout(&out)
    );
}

#[test]
fn search_works_without_an_index() {
    // Search reads the archives, so a missing index is irrelevant to it. This
    // is the observable half of the decision in #38.
    let (home, _p, root) = archived(&["the payment orchestrator"]);
    fs::remove_file(root.join(".recall/index.db")).expect("delete index");

    let out = recall_in(&root, home.path(), &["search", "orchestrator"]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("1 session matching"));
    // And it did not quietly rebuild one to answer.
    assert!(
        !root.join(".recall/index.db").exists(),
        "search created an index it does not need"
    );
}

#[test]
fn searching_without_init_says_what_to_do() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");

    let out = recall_in(project.path(), home.path(), &["search", "anything"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("recall init"));
}

#[test]
fn an_empty_archive_matches_nothing_without_complaining() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    let out = recall_in(&root, home.path(), &["search", "anything"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("No session matches"));
}

#[test]
fn matching_ignores_case() {
    let (home, _p, root) = archived(&["The Payment Orchestrator"]);

    for q in ["payment orchestrator", "PAYMENT ORCHESTRATOR", "PaYmEnT"] {
        let text = stdout(&recall_in(&root, home.path(), &["search", q]));
        assert!(text.contains("session matching"), "{q:?}:\n{text}");
    }
}

#[test]
fn a_match_is_counted_once_per_event_not_once_per_word() {
    // A term repeated within one message is one matching event. Counting every
    // occurrence would make a single verbose message look like a hundred hits.
    let repeated = "parser ".repeat(200);
    let (home, _p, root) = archived(&[&repeated, "parser mentioned once"]);

    let text = stdout(&recall_in(&root, home.path(), &["search", "parser"]));
    assert!(text.contains("2 sessions matching"), "{text}");
    assert!(text.contains("(2 matches)"), "{text}");
}

#[test]
fn a_session_matching_many_times_is_summarised_not_dumped() {
    // One session matching in hundreds of separate events must not bury the
    // others in the listing.
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical");
    assert!(recall_in(&root, home.path(), &["init"]).status.success());

    let slug = root.display().to_string().replace('/', "-");
    let dir = home.path().join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("create project directory");

    let mut body = String::new();
    for n in 0..200 {
        body.push_str(&format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-01T12:00:00.000Z\",\"cwd\":\"{p}\",\"message\":{{\"role\":\"user\",\"content\":\"parser question {n}\"}}}}\n",
            p = root.display()
        ));
    }
    let path = dir.join("busy.jsonl");
    fs::write(&path, body).expect("write session");
    make_settled(&path);
    claude_session(home.path(), &root, "quiet", 2, "parser mentioned once");
    assert!(recall_in(&root, home.path(), &["sync"]).status.success());

    let text = stdout(&recall_in(&root, home.path(), &["search", "parser"]));
    assert!(text.contains("more match"), "not summarised:\n{text}");
    assert!(text.contains("2 sessions matching"), "{text}");
    // The busy session must not have printed 200 lines.
    assert!(
        text.lines().count() < 30,
        "the listing was dumped, {} lines",
        text.lines().count()
    );
}
