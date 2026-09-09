# Claude Code session format

Confirmed against a real installation before the adapter was written —
**Claude Code 2.1.215 to 2.1.228 on macOS**, 183,141 records across 37 session
files, zero unparseable lines.

Structure only. No conversation content was extracted, and every fixture in the
test suite is written by hand from what is described here.

## Location

```text
~/.claude/projects/<slugified-cwd>/<session-uuid>.jsonl
```

The project directory is the working directory with separators replaced by `-`:
`/Users/me/Documents/Work` becomes `-Users-me-Documents-Work`. Session files are
UUID-named JSON Lines.

**That encoding cannot be reversed.** A project whose own path contains a hyphen
is indistinguishable from one with a separator there:

```text
slug:    -Users-me-Documents-Work-ME-OPEN-SOURCE-recall
decodes: /Users/me/Documents/Work/ME/OPEN/SOURCE/recall     wrong
actual:  /Users/me/Documents/Work/ME/OPEN-SOURCE/recall
```

So the directory name is never trusted for the project path. Discovery reads the
`cwd` recorded inside the session — exact, and present on every content record —
and falls back to the decoded name only when no record supplies one. Encoding
the project root and comparing slugs was considered and rejected: it is exact in
one direction and ambiguous in the other, since `/w/foo-bar` and `/w/foo/bar`
encode identically.

`projects/` also holds `.json`, `.md` and `.txt` files, so discovery filters on
extension. Assuming everything under `projects/` is a session would archive
things that are not.

### Sub-agent transcripts

Work done by sub-agents is stored separately, beside the session it belongs to:

```text
~/.claude/projects/<slugified-cwd>/<session-uuid>/subagents/<task-uuid>.jsonl
```

**This is most of the data.** In the installation this adapter was written
against, 121 of 158 session files were sub-agent transcripts. Discovering only
the top-level files silently drops three quarters of the archived work — which
is exactly what the first version of discovery did, until it was checked against
a real installation.

They are not separate sessions. Their content records carry
`isSidechain: true`, and the `sessionId` inside them is the **parent's**, not
their own filename — the filename is the task's id. So they are attached to the
session they belong to rather than reported alongside it.

They also introduce a record type not seen at the top level: `fork-context-ref`.

## Records

One JSON object per line. **21 distinct `type` values**, of which four carry
conversation:

| type | share of records | carries |
|------|------------------|---------|
| `assistant` | 52,679 | model, content blocks |
| `user` | 30,654 | content, `toolUseResult` |
| `attachment` | 28,363 | pasted or attached material |
| `system` | 1,345 | meta and hook events |

The other seventeen are Claude Code's own state rather than transcript:
`ai-title`, `custom-title`, `mode`, `permission-mode`, `agent-name`,
`agent-setting`, `worktree-state`, `pr-link`, `atis-latch`, `bridge-session`,
`file-history-delta`, `file-history-snapshot`, `queue-operation`, `relocated`,
`cost-state`, `history-suppression`, `last-prompt`.

An adapter that treated every record as a message would archive a great deal of
noise, so unknown types are skipped rather than swept into the transcript.

## Fields on every content record

`uuid`, `parentUuid`, `timestamp` (ISO-8601, `Z`), `cwd`, `sessionId`,
`version`, `gitBranch`, `isSidechain`, `userType`.

Two are better than expected: **`cwd` and `gitBranch` are present on 100% of the
83,343 content records examined**, so the adapter supplies the project path and
the branch directly rather than leaving them to be derived from the repository.

`parentUuid` means records form a tree rather than a flat list. `isSidechain`
marks sub-agent conversations — none appeared in this sample, but the field
exists and is worth preserving.

## Content

`message.content` is **either a string or an array of blocks**. Both occur:
1,457 strings against 29,199 arrays on user records. Anything reading only one
shape will silently drop the other.

| block | fields | count |
|-------|--------|-------|
| `text` | `text` | 11,154 |
| `tool_use` | `id`, `name`, `input`, `caller` | 29,139 |
| `tool_result` | `tool_use_id`, `content`, `is_error` | 29,139 |
| `thinking` | `thinking`, `signature` | 12,449 |

`tool_result.content` is string-or-array as well.

`message.model` on `assistant` records is the model identifier.

### Extended thinking

12,449 `thinking` blocks appeared in the sample. Recall's `SessionEvent` has no
variant for them, so they are preserved as
`Other { provider_kind: "thinking" }` — kept rather than dropped, because
dropping is the one thing an archive must not do, and because "what was the
model reasoning about" is exactly the kind of thing someone comes back for.

Whether extended thinking earns its own variant is worth revisiting once a
second provider records something similar.

## `toolUseResult`

Present on 29,137 user records, and the richest field in the format. It is where
commands and file changes are actually derivable from — not from the tool call,
which only records what was requested.

| shape | records | means |
|-------|---------|-------|
| `stdout`, `stderr`, `interrupted` | 19,623 | a command ran |
| `filePath`, `structuredPatch`, `originalFile`, `userModified` | 5,077 | a file was edited |
| `oldString`, `newString`, `replaceAll` | 3,606 | a string replacement |

**It is not always an object.** When a tool fails or the user rejects it, Claude
Code writes a bare string here instead — `"User rejected tool use"`, or the
tool's error text. That accounted for **1,061 records** in the sample, every one
of them a `user` record carrying real conversation.

This is worth stating plainly because it is the kind of thing fixtures never
catch: a parser that types `toolUseResult` as an object parses every synthetic
example perfectly and silently drops one record in every 180 of the real
archive. It was found by parsing an actual installation and noticing the count
did not match.

## Attachments

`attachment` records are mostly **Claude Code talking to itself**, not things a
person or a tool produced:

| attachment type | records | |
|---|---|---|
| `total_tokens_reminder` | 18,254 | internal |
| `task_reminder` | 3,188 | internal |
| `edited_text_file` | 910 | real content |
| `queued_command` | 317 | real content |
| `environment`, `file`, … | ~400 | real content |

Reminders outnumber real attachments by more than ten to one. Archiving them
would bury the conversation in bookkeeping, so the adapter skips a documented
set of internal types and preserves everything else with its text.

The list is a **deny-list on purpose**: an attachment type not on it is kept, so
a new one added by a future release is preserved rather than silently lost.

## What the adapter can and cannot supply

| Recall field | From | Available |
|---|---|---|
| user messages | `user` records | yes |
| assistant responses | `assistant` records | yes |
| tool calls and results | `tool_use` / `tool_result` blocks | yes |
| commands executed | `toolUseResult.stdout` / `stderr` | yes, derived |
| files changed | `toolUseResult.filePath` | yes, derived |
| session start and end | first and last `timestamp` | derived |
| model | `message.model` | yes |
| project path | `cwd` | yes |
| git branch | `gitBranch` | yes |
| commit at start / end | — | no, not recorded |

The last row is the honest gap: Claude Code records the branch but not the
commit, so `commit_at_start` and `commit_at_end` stay empty until #31 derives
them from the repository itself.

## Stability

Six versions appeared in this sample and the shape was consistent across all of
them. That is not a guarantee. The adapter ignores unknown record types and
unknown fields, so a new one is skipped rather than fatal — which is the only
thing that makes a format nobody here controls safe to depend on.
