# Recall

**Local memory for AI coding sessions.**

Recall is local infrastructure for preserving AI coding session history. It is not
another AI agent.

AI coding agents are becoming an integral part of software development, but their sessions
are temporary. A long debugging session, an architectural discussion, an implementation
plan, or hours of investigation can disappear into an agent's session history — especially
when you run out of credits, switch models, change machines, or start a new session.

**Recall preserves that history locally.**

Recall is an open-source, local-first CLI, written in Rust, that discovers and archives AI
coding sessions from tools such as Claude Code, Codex, and other AI coding agents. It
stores conversations in a provider-independent format, compresses them locally, and
associates them with the projects and Git repositories where they happened.

The goal is simple:

> **Your AI may forget the session. Your project shouldn't.**

---

> **Status: early development.** Recall is pre-1.0 and under active construction. The CLI
> surface, archive layout, and index schema described here are the design being built
> toward, and will change before 1.0. Not yet ready for production archives.

---

## What Recall Does

Recall acts as a persistent memory layer between your AI coding sessions and your
codebase.

Instead of relying entirely on an AI provider's session history, Recall creates a local
archive of the work that happened during those sessions.

Captured from the agent's own session record:

- Your messages and the assistant's responses
- Tool calls and tool results
- Commands the agent executed
- Files referenced by agent activity
- Session start and end timestamps
- AI provider and model identifiers

Derived by Recall from the surrounding environment:

- Git repository, branch, and commit
- Project association
- Archive metadata — session identifier, size, integrity checksum

How much of the first list an adapter can supply depends on what the provider's format
records. Recall preserves what is present and leaves the rest absent rather than guessing,
so a session never claims more than its source actually contained.

The original conversation remains available locally, while Recall provides a consistent
way to discover and retrieve it.

## Why Recall?

Imagine spending three hours with an AI agent on a complicated refactor.

```text
Design the architecture
        ↓
Investigate the existing implementation
        ↓
Try approach A
        ↓
Reject approach A
        ↓
Implement approach B
        ↓
Debug an issue
        ↓
Run out of credits
```

You open another AI agent. The new session doesn't know:

- what you already investigated
- which approaches you rejected
- why certain architectural decisions were made
- what files were changed
- what remains unfinished
- where the previous agent stopped

Recall gives you a persistent record of that work.

```text
Previous AI session
        ↓
       Recall
        ↓
Local conversation archive
        ↓
New AI session
        ↓
Continue the work
```

## Local First

Recall is designed around a simple principle:

**Your AI conversations should remain yours.**

Recall does not require a cloud service to store your conversations. Your session history
stays on your machine and inside your project environment.

- No account is required.
- No external database is required.
- No conversation is uploaded to a Recall server. There is no Recall server.

The archive can be stored locally, ignored by Git by default, or explicitly committed when
you decide that conversation history should become part of the project's history.

## Provider Independent

Recall is not tied to a particular AI provider.

AI coding agents already maintain their own session histories. Recall provides a unified
layer for working with that history.

```text
Claude Code ──┐
              │
Codex ────────┤
              ├──> Recall
Gemini CLI ───┤
              │
Other agents ─┘
```

Each provider exposes its own session format through an adapter, and Recall converts that
information into a common internal representation. The rest of Recall does not need to
know whether a session came from Claude, Codex, Gemini, or another agent.

Adapters are **read-only**. Recall never modifies, moves, or deletes an agent's own
session files.

## Local Archive

Recall maintains its own project-level archive of sessions:

```text
.recall/
├── config.toml                     format version and project settings
├── sessions/
│   └── 2026/09/08/
│       ├── session-abc.zst
│       └── session-def.zst
├── tmp/                            staging for atomic writes
└── index.db                        derived metadata index
```

Conversations are stored compressed, to keep the archive small while retaining the
original information. Metadata is indexed separately, so searching does not require
loading every conversation.

Sessions are filed by their start date in UTC, so the same session lands in the same
place regardless of the machine's timezone. The index is derived data — deleting it
loses nothing, because it rebuilds from the archives.

The full specification, including which paths Recall owns and which it will never
touch, is in [`docs/archive-layout.md`](docs/archive-layout.md).

## Git Awareness

AI coding sessions are closely connected to the state of the repository they operate on,
so Recall treats Git as part of the session record. A session can be associated with:

```text
Project
Repository
Branch
Commit at session start
Commit at session end
Files referenced during the session
Session start
Session end
AI provider
AI model
```

Repository facts come from inspecting Git, not from the conversation. What actually
changed on disk is answered by the diff between the session's start and end commits; the
transcript only tells you which files the agent touched while working.

This makes it possible to answer questions such as:

> Which AI session worked on this branch?
>
> What conversations happened before this commit?
>
> Which sessions touched this file?
>
> What was the last AI session on this project?

The long-term goal is to make AI development history as discoverable as Git history.

## Search

Once sessions are stored locally, Recall provides fast access to previous work:

```bash
recall search "payment orchestrator"
recall search "database round trips"
```

Instead of manually navigating provider-specific session histories, you search across your
project's archived AI sessions.

## Installation

Recall is a single binary with no runtime dependencies.

**From source** (the only option while pre-release):

```bash
git clone https://github.com/codewithcobby/recall.git
cd recall
cargo install --path crates/recall-cli
```

Requires a stable Rust toolchain — install one via [rustup](https://rustup.rs).

To verify your environment without installing the binary:

```bash
cargo run -p recall-cli -- --help
```

**From crates.io** (once published):

```bash
cargo install recall-cli
```

## Simple CLI

Recall is intended to feel like a normal developer tool.

```bash
recall init
```

Initialize Recall for a project.

```bash
recall sync
```

Discover and archive new AI sessions.

```bash
recall sessions
```

List previous sessions.

```bash
recall show <session>
```

Inspect a specific session.

```bash
recall search "authentication"
```

Search previous AI work.

The interface should remain simple enough that Recall becomes part of the normal
development workflow rather than another system developers have to maintain.

## Designed for Agent Continuity

The immediate goal of Recall is **preservation and retrieval**, not AI summarization.
Recall does not need an LLM to perform its core functionality.

The first layer is deterministic:

```text
Discover
   ↓
Capture
   ↓
Normalize
   ↓
Compress
   ↓
Index
   ↓
Retrieve
```

AI-powered capabilities can be added later, but the underlying project memory should
remain useful even without an AI model. This separation keeps the core lightweight, local,
and provider-independent.

## Architecture

Recall is a Cargo workspace. `recall-core` owns the domain types and the traits the other
crates implement, so dependencies point inward, toward the core — and nothing
provider-specific ever reaches it.

```text
crates/
├── recall-cli/        Binary. Argument parsing, output rendering, exit codes.
├── recall-core/       Normalized session model and the capture pipeline's contracts.
├── recall-adapters/   One module per AI coding agent. Discovery and parsing only.
├── recall-store/      Archive layout, compression, integrity of .recall/sessions.
└── recall-index/      Metadata index and search over .recall/index.db.
```

```text
                    recall-cli
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
   recall-adapters  recall-store   recall-index
        └───────────────┼───────────────┘
                        ▼
                   recall-core
```

`recall-core` depends on none of the crates below it in that diagram. `recall-cli` is the
only crate that knows which concrete implementations exist, which keeps the core reusable
by consumers that are not the CLI.

## Building from Source

```bash
git clone https://github.com/codewithcobby/recall.git
cd recall

cargo build --workspace
cargo test --workspace

cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

## Future Direction

Recall can eventually become more than a conversation archive. The long-term vision is a
persistent memory layer for AI-assisted software development:

- Cross-agent session continuity
- Automatic project context files
- `AGENTS.md` integration
- `CLAUDE.md` integration
- Session timelines
- Semantic search
- Architectural decision tracking
- Detection of unfinished work
- Session-to-commit relationships
- Cross-machine synchronization
- Optional local AI summarization
- MCP integration
- Agent handoff workflows

For example:

```bash
recall continue
```

could eventually identify the most relevant previous sessions and prepare the context
necessary for another AI agent to continue the work.

The AI model may change. The coding agent may change. The developer may start a completely
new session.

**The project's memory remains.**

## Philosophy

Recall is built around three principles.

### Local

Your conversations and project history should remain under your control.

### Open

The storage format, adapters, and tooling should be open source and extensible.

### Persistent

AI sessions are temporary. The knowledge generated while building software should not be.

## Contributing

Contributions are welcome — especially provider adapters for AI coding agents Recall does
not yet support.

Read [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md) first. It defines the branch
naming, commit format, pull request process, Rust standards, and the data-safety rules
that reviewers enforce. Work is tracked through GitHub Issues; open one before writing
code.

Adapter authors should start from the **Writing a Provider Adapter** section of that guide
and the `[adapter]` issue template.

## Security

Recall archives may contain proprietary source code and, occasionally, credentials that
were pasted into a prompt. Treat `.recall/` with the same care as the source tree it sits in.

Report vulnerabilities privately — see [`.github/SECURITY.md`](.github/SECURITY.md). Never
open a public issue for an exploitable vulnerability.

## License

MIT. See [LICENSE](LICENSE).

---

**Recall — because your AI coding sessions shouldn't disappear when the conversation
ends.**
