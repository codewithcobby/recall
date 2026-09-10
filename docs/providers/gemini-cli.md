# Gemini CLI session format

Confirmed against a real installation — **Gemini CLI 0.59.0 on macOS**.

Structure only. No conversation content was extracted, and every fixture in the
test suite is written by hand from what is described here.

> **The conversation records in this document are not yet confirmed.** The
> layout, the header record and the record envelope below were read from a real
> session file. The *message* shapes were not: see [Not yet
> confirmed](#not-yet-confirmed).

## Location

```text
~/.gemini/tmp/<project-name>/chats/session-<timestamp>-<id8>.jsonl
~/.gemini/tmp/<project-name>/.project_root      absolute project path
~/.gemini/tmp/<project-name>/logs.json          the CLI's own log, not transcript
~/.gemini/projects.json                         absolute path to short name
```

On Windows the same tree sits under `%USERPROFILE%\.gemini`.

`tmp/` is the Gemini CLI's own name for it, which undersells the contents: the
transcripts under here are the only record of a conversation.

`chats/` appears only once a session has been started. A project directory that
exists without one is normal — the CLI creates the project directory as soon as
it runs there.

### `~/.gemini` belongs to more than one product

**Antigravity** — a different Google product, invoked as `agy` — keeps its own
state in the same directory:

```text
~/.gemini/antigravity/
~/.gemini/antigravity-cli/conversations/<uuid>.db
~/.gemini/antigravity-ide/conversations/<uuid>.db
```

Those are SQLite databases holding protobuf payloads, not Gemini CLI sessions.
Reading them as though they were would archive one product's transcripts under
another's name. Discovery therefore scopes itself to `tmp/` rather than walking
`~/.gemini`.

### The directory name is not the project

The Gemini CLI names a project directory after the **last component** of the
project's path:

```text
/Users/me/Work/STACKWARES/campuseats   →   tmp/campuseats/
```

That cannot be reversed, and it collides: `/w/api` and `/other/api` both become
`api`. `.project_root` beside the sessions holds the absolute path exactly, and
that is what discovery reads. `projects.json` records the same mapping from the
other direction.

### The filename holds only part of the id

```text
session-2026-09-10T11-25-be9de464.jsonl
        └──────┬──────┘ └───┬───┘
            timestamp    first 8 characters of the session id
```

The real id is on the file's own header record —
`be9de464-9658-4c1b-a810-664ecc8b06e8` for the example above. Eight characters
is not enough to identify a session: two sessions sharing that prefix would
collide, and the prefix traces back to nothing the provider knows.

Unlike Codex, there is no full id in the filename to fall back on, so a session
whose header cannot be read is skipped rather than archived under a guess.

## Records

One JSON object per line.

### The header

The first line, and the only record with `kind`:

```json
{"sessionId": "<uuid>", "projectHash": "<sha256 hex>",
 "startTime": "<ISO 8601>", "lastUpdated": "<ISO 8601>", "kind": "main"}
```

`projectHash` is a hash of the project path. `.project_root` is preferred over
it because it is the path itself rather than something to be matched against.

### Message records and `$set`

After the header the file alternates between two shapes. A message record:

```json
{"id": "<uuid>", "timestamp": "<ISO 8601>", "type": "<kind>", "content": ...}
```

and a `$set` record, which carries the whole accumulated conversation:

```json
{"$set": {"messages": [ ...every message so far... ], "lastUpdated": "<ISO 8601>"}}
```

**This matters for parsing.** `$set` is a snapshot, not an increment: the same
message appears both as its own record and inside every later `$set`. Reading
both would archive a conversation many times over — the same trap Codex's
`event_msg` stream set, in a different shape. Which of the two an adapter should
read is settled in #46.

`type` was `info` on every message in the session read here — the CLI's own
notices rather than conversation, because the session never reached the model.

## Not yet confirmed

The session this was written from never completed a model turn: Google has
withdrawn Gemini Code Assist for individual Google sign-in on this client, and
the run failed at authentication.

So the following are **not** established, and #46 must not assume them:

- what a user message, an assistant response, a tool call and a tool result look like
- the `type` values that carry conversation
- whether `content` is a string, a list of parts, or both
- whether tool calls and results are separate records or nested
- whether the model identifier is recorded anywhere
- whether a git branch is recorded

What is established: the layout, the header, the record envelope, and the
`$set` duplication problem.

## Stability

Read from Gemini CLI 0.59.0 only, so nothing here is known about how the format
moves between releases. The adapter names only the fields it uses and preserves
what it does not recognise, the same as the other two.
