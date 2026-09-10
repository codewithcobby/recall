# Contributing to Recall

Recall is a local-first CLI, written in Rust, that discovers and archives AI coding
sessions. Its archive is often the only surviving copy of a conversation, so the bar for
changes is correctness and durability before features.

This guide is the source of truth for how work is tracked, branched, committed, and
reviewed here. It applies to everyone — maintainers, outside contributors, and autonomous
agents. Pull requests that do not follow this process may be rejected regardless of code
quality — maintainers can grant a reasonable exception, but the exception is agreed on the
issue, not assumed in the pull request.

---

## Table of Contents

- [Contributing to Recall](#contributing-to-recall)
  - [Table of Contents](#table-of-contents)
  - [Project Principles](#project-principles)
  - [Prerequisites](#prerequisites)
  - [Repository Layout](#repository-layout)
    - [Dependency direction](#dependency-direction)
  - [Contribution Workflow](#contribution-workflow)
    - [1. Check for a duplicate](#1-check-for-a-duplicate)
    - [2. Open an issue](#2-open-an-issue)
    - [3. Branch from the latest `dev`](#3-branch-from-the-latest-dev)
    - [4. Implement](#4-implement)
    - [5. Validate](#5-validate)
    - [6. Open a pull request](#6-open-a-pull-request)
    - [7. Review](#7-review)
  - [Issue Requirements](#issue-requirements)
  - [Branch Conventions](#branch-conventions)
    - [Allowed types](#allowed-types)
    - [Validation pattern](#validation-pattern)
    - [Examples](#examples)
    - [Renaming](#renaming)
  - [Commit Standards](#commit-standards)
    - [Validation pattern](#validation-pattern-1)
    - [Examples](#examples-1)
    - [Prohibited](#prohibited)
  - [Pull Request Process](#pull-request-process)
    - [Size guidelines](#size-guidelines)
    - [Merge strategy](#merge-strategy)
  - [Stacked Pull Requests](#stacked-pull-requests)
  - [Rust Code Standards](#rust-code-standards)
    - [Prohibited](#prohibited-1)
  - [Testing Requirements](#testing-requirements)
  - [Writing a Provider Adapter](#writing-a-provider-adapter)
  - [Data Safety Rules](#data-safety-rules)
  - [Documentation Requirements](#documentation-requirements)
  - [Code Review Standards](#code-review-standards)
  - [Security](#security)
  - [Questions](#questions)

---

## Project Principles

Every change is measured against these. A patch that violates one needs an explicit,
argued exception in the pull request, not silence.

| Principle                | What it forbids in practice                                                                         |
| ------------------------ | --------------------------------------------------------------------------------------------------- |
| **Local**                | No network calls, no telemetry, no account, no required external service in the core path.          |
| **Provider independent** | No provider-specific type, field, or branch outside `recall-adapters`.                              |
| **Deterministic**        | The discover → capture → normalize → compress → index → retrieve pipeline never requires an LLM.    |
| **Non-destructive**      | Provider session files are read-only. Recall never modifies, moves, or deletes them.                |
| **Durable**              | Archive layout and index schema changes are backward compatible or shipped with a tested migration. |

---

## Prerequisites

- Rust stable, matching `rust-toolchain.toml` when present. Install via [rustup](https://rustup.rs).
- `rustfmt` and `clippy` components: `rustup component add rustfmt clippy`.
- The [GitHub CLI](https://cli.github.com/) (`gh`), authenticated.
- Git configured with a real `user.name` and `user.email`.

Verify your environment before your first PR:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

---

## Repository Layout

Recall is a Cargo workspace. Know which crate you are changing before you start — it
determines your commit scope and your reviewer.

```
crates/
├── recall-cli/        Binary. Argument parsing, output rendering, exit codes.
│                      Composes the concrete implementations and wires them together.
├── recall-core/       Domain types and the traits the other crates implement — the
│                      normalized session model and the capture pipeline's contracts.
│                      Provider-agnostic, and depends on none of the crates below.
├── recall-adapters/   One module per AI coding agent. Discovery and parsing only.
├── recall-git/        Read-only detection of the repository a session ran against.
├── recall-store/      Archive layout, compression, integrity of .recall/sessions.
└── recall-index/      Metadata index and search over .recall/index.db.
```

### Dependency direction

`recall-core` owns the contracts; the infrastructure crates implement them. Dependencies
therefore point *inward*, toward core:

```text
                    recall-cli
                        │            composes and wires implementations
        ┌──────────┬───────┴───────┬──────────┐
        ▼          ▼               ▼          ▼
  recall-adapters  recall-git  recall-store  recall-index
        │          │               │          │
        └──────────┴───────┬───────┴──────────┘
                        ▼
                   recall-core
                                     owns the domain types and traits
```

Two things follow, and both are design errors rather than shortcuts:

- `recall-core` must not depend on `recall-adapters`, `recall-git`, `recall-store`, or
  `recall-index`.
  It defines what a session is and what a store or an index must be able to do; it never
  reaches for a concrete one.
- Nothing outside `recall-adapters` may contain a provider-specific type, field, or
  branch.

`recall-cli` is the only crate that knows which concrete implementations exist. That
keeps the core reusable by a future non-CLI consumer without dragging the binary's
choices along with it.

> The workspace does not exist yet. This graph is the target the first `Cargo.toml` must
> match, and reviewers check new dependency edges against it.

---

## Contribution Workflow

```
Issue → Branch from dev → Commits → Validation → Pull Request → Review → Merge to dev
```

`dev` is the integration branch and the target of every pull request. `main` holds
released code. Both are protected; direct pushes are rejected.

### 1. Check for a duplicate

```bash
gh issue list --repo codewithcobby/recall --search "<keywords>" --state all
```

### 2. Open an issue

Use the template that fits. Blank issues are disabled. Record the number — everything
downstream references it.

### 3. Branch from the latest `dev`

```bash
git fetch origin
git checkout -b <type>/<issue-number>-<slug> origin/dev
```

Always branch from `origin/dev`, never from a stale local copy.

### 4. Implement

Small, logical commits. Each commit should build and pass tests on its own.

### 5. Validate

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

All three must pass. Never open a pull request knowing you introduced a failure.

### 6. Open a pull request

```bash
git push -u origin <branch-name>
gh pr create --base dev
```

Fill in the template completely. Delete sections that do not apply rather than leaving
them blank.

### 7. Review

Respond to every comment. Push fixes as new commits — do not force-push during review;
it destroys the reviewer's diff-since-last-look.

---

## Issue Requirements

- Every change is tracked by an issue opened before the code is written.
- Issues use the provided forms. Blank issues are disabled.
- Titles carry the prefix their template sets: `[bug]`, `[feature]`, `[adapter]`,
  `[perf]`, `[security]`, `[docs]`.
- An issue must contain enough for someone else to start work without asking you a
  question.
- Never paste real conversation content, source code, credentials, or absolute paths from
  your own machine into an issue. Redact or synthesize.

---

## Branch Conventions

All branches follow:

```
<type>/<issue-number>-<short-slug>
```

### Allowed types

| Type       | Purpose                                     |
| ---------- | ------------------------------------------- |
| `feature`  | New functionality                           |
| `fix`      | Bug fixes                                   |
| `chore`    | Maintenance, dependency updates             |
| `docs`     | Documentation changes                       |
| `refactor` | Code restructuring without behaviour change |
| `perf`     | Performance improvements                    |
| `test`     | Test additions or modifications             |
| `build`    | Build system and Cargo changes              |
| `ci`       | CI/CD pipeline changes                      |

### Validation pattern

```regex
^(feature|fix|chore|docs|refactor|perf|test|build|ci)/[0-9]+-[a-z0-9-]+$
```

Only those nine types are valid. `bug/`, `bugfix/`, `hotfix/`, `feat/`, and `security/`
are **not** branch types — a bug goes under `fix`, and a security change goes under `fix`,
or `chore` when it is hardening with no behaviour change. The slug is lowercase
alphanumeric and hyphens only.

### Examples

```
feature/42-codex-adapter
fix/123-zstd-frame-truncation
perf/88-index-batch-inserts
docs/15-adapter-authoring-guide
```

### Renaming

Get the name right when you create the branch. Renaming a branch after its pull request
is open can close the PR instead of retargeting it. If a rename is unavoidable, expect to
recreate the PR and leave a comment on the closed one pointing at its replacement.

---

## Commit Standards

[Conventional Commits](https://www.conventionalcommits.org/), with a mandatory scope.

```
<type>(<scope>): <subject>
```

- **Type**: `feat`, `fix`, `chore`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`.
- **Scope**: required. The crate or subsystem touched — `cli`, `core`, `adapter`,
  `store`, `index`, `search`, `git`, `config`, `deps`, or a specific adapter such as
  `adapter-claude`.
- **Subject**: imperative mood, lowercase, no trailing period, at most 72 characters.
- **Body**: optional, after a blank line. Explains why, not what — the diff already says
  what.
- **Footer**: `BREAKING CHANGE: <description>` when the CLI surface, archive layout,
  index schema, or a public API changes.

### Validation pattern

```regex
^(feat|fix|chore|docs|refactor|perf|test|build|ci)\([a-z0-9-]+\): .{1,72}$
```

### Examples

```
feat(adapter-codex): discover sessions under the CLI history directory
fix(store): reject truncated zstd frames instead of returning empty content
perf(index): batch metadata inserts into a single transaction
refactor(core): move session normalization behind a trait
docs(readme): document the .recall archive layout
```

### Prohibited

- No `Signed-off-by:` trailers.
- No scope-less commits — bare `fix: ...` is rejected.
- No `TODO`, `FIXME`, `HACK`, or `XXX` in commit messages.
- No merge commits inside a feature branch. Rebase onto `dev` instead.

---

## Pull Request Process

1. **Title** follows the same Conventional Commits format as a commit.
2. **Issue reference** is required: `Closes #<n>`, `Fixes #<n>`, or `Relates to #<n>`.
3. **One issue per pull request.** Split multi-concern work — see
   [Stacked Pull Requests](#stacked-pull-requests).
4. **Template** filled out completely, including the archive-safety section.
5. **CI green** before review is requested.
6. **Target `dev`**, except within a stack.

### Size guidelines

| Size        | Lines changed | Expectation                                          |
| ----------- | ------------- | ---------------------------------------------------- |
| Small       | < 100         | Preferred. Fast review.                              |
| Medium      | 100–300       | Acceptable with a clear summary.                     |
| Large       | 300–500       | Justify it. Consider splitting.                      |
| Extra large | > 500         | Agree the shape with a maintainer before writing it. |

A new provider adapter is naturally large; open the `[adapter]` issue and settle the
format questions there before implementing.

### Merge strategy

- Into `dev`: **squash merge**.
- `dev` into `main` for a release: **merge commit**, to preserve history.
- Branch is deleted after merge.
- Contributors never merge their own pull requests unless a maintainer says to.

---

## Stacked Pull Requests

**Most contributions do not need this section.** One issue is one branch and one pull
request off `dev` — that is the default path, and a small fix should never require
learning stack tooling.

Stacks are for dependent, multi-issue work, where issue N+1 builds on issue N — a phased
refactor, or groundwork plus the features that sit on it:

```text
#10 session model
      ↓
#11 Claude adapter
      ↓
#12 archive store
      ↓
#13 sync command
```

When work does span more than one issue, use **GitHub's native stacked pull requests**.
Not Graphite, not `spr`, not `ghstack`, not hand-chained `gh pr create --base`.

```bash
gh extension install github/gh-stack

gh stack init -b dev feature/10-session-model
# implement, commit
gh stack add feature/11-claude-adapter
# implement, commit
gh stack view
gh stack submit          # pushes branches, creates PRs with the right bases (draft by default)
```

The bottom pull request targets `dev`; each one above targets the branch below it. Order
the stack by dependency — shared groundwork at the bottom. Every branch still follows the
naming convention, every pull request still references its own issue, and validation runs
before `gh stack submit`.

After anything in the stack merges, run `gh stack sync` before continuing. Exit code 9
means stacked pull requests are not enabled for this repository — report it rather than
falling back to another tool.

---

## Rust Code Standards

- **Formatting**: `cargo fmt` output, unmodified. CI checks it.
- **Lints**: `cargo clippy --all-targets --all-features -- -D warnings` is clean. Silence
  a lint only with `#[allow(...)]` carrying a comment that justifies it — never a
  crate-wide blanket allow.
- **Errors**: libraries return typed errors (`thiserror`); the binary uses `anyhow` at the
  top level. A malformed session file is an error value, never a panic.
- **No panics in library code**: no `unwrap()` or `expect()` on anything that depends on
  input, filesystem state, or the environment. In tests they are fine.
- **No `unsafe`**: crates declare `#![forbid(unsafe_code)]`. Removing that needs a
  maintainer's agreement on the issue first.
- **Public API is documented**: every public item carries a `///` doc comment explaining
  intent. `cargo doc` emits no new warnings.
- **Dependencies**: adding one needs justification in the pull request — what it does, why
  the standard library will not do, its licence, and its transitive weight. Prefer no
  dependency, then a small well-maintained one. Pin to a specific version.
- **Portability**: no hardcoded path separators or home directories. Provider locations
  are resolved per platform.
- **Streaming over slurping**: session files can be hundreds of megabytes. Read
  incrementally; do not load a whole transcript into memory to count its records.

### Prohibited

- `TODO`, `FIXME`, `HACK`, `XXX` in committed code — open an issue instead.
- Dead code, unused imports, commented-out blocks.
- Hardcoded secrets, credentials, or machine-specific paths.
- `println!` for diagnostics in library crates. Use the logging facade; user-facing output
  belongs in `recall-cli`.

---

## Testing Requirements

- New functionality ships with unit tests covering the success, edge, and error cases.
- Every bug fix ships with a regression test that fails before the fix.
- Tests are deterministic: seed randomness, inject clocks, never assert on wall-clock time
  or on filesystem iteration order.
- Filesystem tests use a temporary directory. A test that touches the real `~` or a real
  provider directory will be rejected.
- Adapter tests run against **synthetic fixtures** committed under the adapter's
  `tests/fixtures/`. Fixtures are hand-written or fully redacted — never a real
  transcript.
- Cross-crate behaviour gets an integration test: end-to-end `init → sync → search`
  against a fixture archive.
- Changes to the archive format or index schema include a test that reads an archive
  written by the previous format.
- Aim for meaningful coverage of the code you changed, and cover every error branch you
  introduce. Coverage percentage is a signal, not a target — a test that asserts nothing
  is worse than no test.

---

## Writing a Provider Adapter

An adapter teaches Recall to read one agent's session history. Open an `[adapter]` issue
and settle the format before writing code.

The contract:

1. **Read-only.** Open provider files read-only. Never write, move, rename, or delete
   anything in a provider's directory.
2. **Discovery is explicit.** Resolve session locations per platform. Never scan a whole
   home directory.
3. **Parse defensively.** Input is untrusted and possibly hostile. A truncated, mid-write,
   or malformed file yields an error for that session, and the rest of the sync continues.
   One bad file never fails the run, and no input value becomes a filesystem path.
4. **Normalize completely.** Map into Recall's session model; do not invent fields.
   Information the format cannot supply is absent, not guessed.
5. **Lossless where it matters.** Preserve tool calls, tool results, commands, and file
   operations. Do not summarize or drop content because it looks noisy.
6. **Stay contained.** Provider-specific types stay inside the adapter module. Nothing
   provider-specific leaks into `recall-core`.
7. **Version the format.** Record which provider version a fixture came from; formats
   change between releases.
8. **Document the layout.** The adapter module's doc comment states where sessions live on
   each platform and how the format is structured.

---

## Data Safety Rules

These are absolute, and reviewers enforce them:

- **Treat provider session files as untrusted input.** Paths, filenames, and metadata
  inside a transcript are data to be recorded, never paths to resolve, read, or write.
  Recall derives its own destinations and verifies they stay inside the archive boundary
  — including when a symlink is in the way. See [SECURITY.md](SECURITY.md).
- **Never write to a provider's session directory.**
- **Never log conversation content** at any level. Log identifiers, counts, paths.
- **Never send anything off the machine.** No network calls, no telemetry, no crash
  reporting in the core path.
- **Never delete archived sessions implicitly.** Destructive operations are explicit,
  confirmed, and scoped.
- **Never break an existing archive.** Format changes are backward compatible or come with
  a migration tested against a populated archive.
- **Never commit real conversation data**, including in fixtures, issues, and pull request
  descriptions.

---

## Documentation Requirements

- `README.md` is updated when setup, usage, commands, or architecture change.
- CLI help text is updated in the same commit as the flag or command it describes.
- New conventions, formats, or schemas are documented under `docs/`.
- Public items carry rustdoc explaining intent and failure modes, not restating the
  signature.
- No redundant comments narrating what the code already says.

---

## Code Review Standards

**Reviewers** verify correctness, data safety, and adherence to the principles above;
confirm tests cover the changed paths; check that documentation moved with the code; and
leave actionable comments with file and line references.

**Authors** respond to every comment, even to acknowledge; push fixes as new commits
rather than force-pushing; and leave conversations for the reviewer to resolve.

**Approval** requires one maintainer approval, green CI, and no unresolved conversations.

---

## Security

Exploitable vulnerabilities go through
[private disclosure](https://github.com/codewithcobby/recall/security/advisories/new),
never a public issue. See [SECURITY.md](SECURITY.md) for scope and expectations.

---

## Questions

If this guide does not cover your situation, open a `[docs]` issue describing the gap.
Ask rather than improvise a process.
