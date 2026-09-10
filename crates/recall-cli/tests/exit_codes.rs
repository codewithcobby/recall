//! Exit codes.
//!
//! A person reads the message; a script reads the code. Both need to tell "no
//! such session" from "the archive is damaged", because one is a typo and the
//! other is data loss.

use std::fs;
use std::path::Path;
use std::process::Command;

fn code(dir: &Path, home: &Path, args: &[&str]) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_recall"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("run recall")
        .status
        .code()
        .expect("an exit code")
}

fn empty_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("home")
}

/// A settled session so sync has something to archive.
fn claude_session(home: &Path, project: &Path, id: &str) {
    let slug = project.display().to_string().replace('/', "-");
    let dir = home.join(".claude/projects").join(slug);
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(
        &path,
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-08T12:00:00.000Z\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":\"x\"}}}}\n",
            project.display()
        ),
    )
    .expect("write");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .expect("backdate");
}

#[test]
fn success_is_zero() {
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    assert_eq!(code(p.path(), home.path(), &["init"]), 0);
    assert_eq!(code(p.path(), home.path(), &["sessions"]), 0);
    assert_eq!(code(p.path(), home.path(), &["--version"]), 0);
}

#[test]
fn an_unusable_command_line_is_two() {
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    assert_eq!(
        code(p.path(), home.path(), &["definitely-not-a-command"]),
        2
    );
    assert_eq!(code(p.path(), home.path(), &[]), 2);
}

#[test]
fn nothing_reports_itself_as_not_built_any_more() {
    // Code 3 means "the command is fine, it just is not built". `recall search`
    // was the last command producing it, and Phase 10 implemented it.
    //
    // The code stays defined and documented — it is an interface, and the next
    // command to be added before it works will use it. What must not happen is
    // a shipped command still claiming it.
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");

    for args in [
        vec!["sync"],
        vec!["sessions"],
        vec!["verify"],
        vec!["search", "anything"],
        vec!["show", "abc"],
    ] {
        assert_ne!(
            code(p.path(), home.path(), &args),
            3,
            "`recall {}` still reports itself unimplemented",
            args.join(" ")
        );
    }
}

#[test]
fn no_archive_here_is_four() {
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    for args in [vec!["sessions"], vec!["sync"], vec!["show", "abc"]] {
        assert_eq!(
            code(p.path(), home.path(), &args),
            4,
            "`recall {}` did not report an uninitialized project",
            args.join(" ")
        );
    }
}

#[test]
fn no_such_session_is_five() {
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    code(p.path(), home.path(), &["init"]);
    assert_eq!(code(p.path(), home.path(), &["show", "zzzzzzzz"]), 5);
}

#[test]
fn an_ambiguous_id_is_six() {
    // Separate from "not found": one is a typo, the other means the user must
    // choose, and a script may want to react differently.
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    let root = p.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "one");
    claude_session(home.path(), &root, "two");
    code(&root, home.path(), &["init"]);
    code(&root, home.path(), &["sync"]);

    assert_eq!(code(&root, home.path(), &["show", ""]), 6);
}

#[test]
fn a_damaged_archive_is_seven() {
    // The code that matters most: this one means data loss, not user error.
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    let root = p.path().canonicalize().expect("canonical");
    claude_session(home.path(), &root, "one");
    code(&root, home.path(), &["init"]);
    code(&root, home.path(), &["sync"]);

    // Corrupt the archive that was just written.
    let mut stack = vec![root.join(".recall/sessions")];
    let mut archive = None;
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|x| x.to_str()) == Some("zst") {
                archive = Some(path);
            }
        }
    }
    let archive = archive.expect("an archive");
    let mut bytes = fs::read(&archive).expect("read");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::write(&archive, &bytes).expect("corrupt");

    let id = archive
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("stem")
        .to_string();
    assert_eq!(code(&root, home.path(), &["show", &id]), 7);
}

#[test]
fn the_codes_are_documented_where_someone_will_look() {
    let home = empty_home();
    let p = tempfile::tempdir().expect("project");
    let out = Command::new(env!("CARGO_BIN_EXE_recall"))
        .arg("--help")
        .current_dir(p.path())
        .env("HOME", home.path())
        .output()
        .expect("run recall");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Exit codes"), "not in --help: {stdout}");
    for line in [
        "4  Recall is not initialized here",
        "7  an archive is damaged",
    ] {
        assert!(stdout.contains(line), "missing {line:?}");
    }
}
