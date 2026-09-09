//! Build a session, write it, read it back, then try to break it.
//!
//! Run with `cargo run --example session_roundtrip`.
//!
//! Phase 2 has no CLI surface — no `recall` command touches these types until
//! `recall sync` in Phase 6 — so this is how a person sees what a stored
//! session actually looks like. `docs/manual-testing.md` walks through the
//! output.
//!
//! It lives in the repository rather than in the guide so it stays compiled:
//! `cargo clippy --all-targets` covers examples, so a change to the types that
//! breaks it fails CI.

use recall_core::{read_session, write_session, FileAction, Provider, Session, SessionEvent};
use time::macros::datetime;

fn main() {
    // 1. Build a session the way an adapter eventually will.
    let provider = Provider::new("claude-code").expect("valid provider name");
    let mut session = Session::new(provider, "abc-123", datetime!(2026-09-08 12:00:00 UTC));
    session.model = Some("claude-opus-5".into());
    session.ended_at = Some(datetime!(2026-09-08 14:30:00 UTC));
    session.project = Some("/home/me/project".into());

    session.events = vec![
        SessionEvent::UserMessage {
            at: Some(datetime!(2026-09-08 12:00:05 UTC)),
            content: "refactor the payment orchestrator".into(),
        },
        SessionEvent::AssistantMessage {
            at: None,
            content: "Reading the current implementation first.".into(),
        },
        SessionEvent::ToolCall {
            at: None,
            name: "read_file".into(),
            arguments: Some(r#"{"path":"src/payments.rs"}"#.into()),
            call_id: Some("call-1".into()),
        },
        SessionEvent::ToolResult {
            at: None,
            call_id: Some("call-1".into()),
            content: "pub fn charge() {}".into(),
            failed: Some(false),
        },
        SessionEvent::Command {
            at: None,
            command: "cargo test".into(),
            exit_code: Some(0),
            output: Some("ok".into()),
        },
        SessionEvent::FileChange {
            at: None,
            action: FileAction::Modified,
            path: "src/payments.rs".into(),
        },
        SessionEvent::Other {
            at: None,
            provider_kind: "system_prompt".into(),
            content: "be concise".into(),
        },
    ];

    println!("== 1. what Recall knows about this session ==");
    println!("  id            {}", session.id);
    println!("  provider      {}", session.provider);
    println!("  provider's id {}", session.provider_session_id);
    println!("  model         {:?}", session.model);
    println!(
        "  archive date  {:?}   <- UTC year/month/day",
        session.archive_date()
    );
    println!(
        "  duration      {:?} minutes",
        session.duration().map(|d| d.whole_minutes())
    );
    println!("  events        {}", session.event_count());

    // 2. Write it out.
    let mut bytes = Vec::new();
    write_session(&mut bytes, &session).expect("writing a session should not fail");
    let text = String::from_utf8(bytes.clone()).expect("the format is UTF-8");

    println!("\n== 2. what that looks like on disk ==");
    for (i, line) in text.lines().enumerate() {
        let label = if i == 0 { "header" } else { "event " };
        println!("  {label} {}", elide(line, 108));
    }

    // 3. Read it back and compare.
    let recovered = read_session(bytes.as_slice()).expect("reading it back should not fail");
    println!("\n== 3. round trip ==");
    println!("  identical to what we wrote: {}", recovered == session);

    // 4. Damaged files are refused, never quietly accepted.
    println!("\n== 4. what happens to a damaged file ==");
    refusal("tampered id", &text.replace("abc-123", "tampered"));
    refusal("truncated file", &text[..text.len() - 30]);
    refusal(
        "future version",
        &text.replacen("\"format\":1", "\"format\":99", 1),
    );
    refusal("empty file", "");

    println!("\nEvery line above should say \"refused\". An \"ACCEPTED\" is a bug.");
}

/// Try to read a deliberately broken session and report what happened.
fn refusal(what: &str, damaged: &str) {
    match read_session(damaged.as_bytes()) {
        Ok(session) => println!(
            "  {what:<16} ACCEPTED as {} events  <- this would be a bug",
            session.event_count()
        ),
        Err(e) => println!("  {what:<16} refused: {e}"),
    }
}

/// Keep a long line readable in a terminal.
fn elide(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let kept: String = line.chars().take(max).collect();
    format!("{kept}…")
}
