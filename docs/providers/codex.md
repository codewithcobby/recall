# Codex session format

Confirmed against a real installation before the adapter was written —
**Codex CLI 0.152.1 on macOS**, 9,354 records across 24 session files, zero
unparseable lines.

Structure only. No conversation content was extracted, and every fixture in the
test suite is written by hand from what is described here.

## Location

```text
~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<timestamp>-<session-uuid>.jsonl
```

On Windows the same tree sits under `%USERPROFILE%\.codex`. Codex calls these
files *rollouts*. One file is one session, and there are no sibling transcripts
to attach — the sub-agent problem that dominates the Claude Code adapter does
not arise here.

**The tree is partitioned by date, not by project.** Nothing in the path says
which repository a session ran against, so the project has to come from inside
the file. Discovery reads the `cwd` on the opening `session_meta` record and
reports no project when there is none, rather than guessing one from the date.

The filename carries the session id, but recovering it needs care: the timestamp
contains hyphens too.

```text
rollout-2026-08-03T13-10-33-019fc7bf-5907-7222-a195-fa7ee3f9556f.jsonl
        └────────┬────────┘ └──────────────────┬─────────────────┘
             timestamp                    session uuid
```

Splitting on the first hyphen gives the wrong answer. The uuid is the trailing
five hyphen-separated groups, checked against the 8-4-4-4-12 hex shape.

Discovery does not rely on this. The `session_id` inside the file is the id
Codex itself considers authoritative, and the filename is only a fallback for a
rollout whose header is missing or truncated — such a file is still reported,
because finding a session and understanding it are separate steps.

The tree is walked to a bounded depth and symlinks are not followed, so a link
planted under `sessions/` cannot walk Recall out of the directory it was told to
read.

## Records

Every line is a JSON object with the same four fields:

```json
{"timestamp": "<ISO 8601>", "ordinal": 0, "type": "<record type>", "payload": {}}
```

`type` names the outer record; the transcript is inside `payload`, which for
`response_item` and `event_msg` carries its own `type`. Counts are from the
installation above.

| Record | Count | What it carries |
| --- | --- | --- |
| `response_item` / `message` | 4,204 | User and assistant messages |
| `event_msg` / `item_completed` | 4,162 | Codex's own UI bookkeeping |
| `event_msg` / `token_count` | 160 | Token usage and rate limits |
| `response_item` / `function_call` | 153 | A tool call, arguments as a JSON string |
| `response_item` / `function_call_output` | 153 | What the tool returned |
| `event_msg` / `task_started` | 106 | Turn boundary |
| `event_msg` / `task_complete` | 104 | Turn boundary |
| `response_item` / `reasoning` | 101 | Model reasoning summaries |
| `turn_context` | 71 | Model, cwd, approval and sandbox policy per turn |
| `event_msg` / `thread_settings_applied` | 52 | Settings bookkeeping |
| `world_state` | 33 | Codex's snapshot of the workspace |
| `session_meta` | 24 | Session header — one per file, written first |
| `response_item` / `web_search_call` | 8 | A web search |
| `response_item` / `custom_tool_call`(`_output`) | 12 | Non-function tool calls |
| `response_item` / `tool_search_call`(`_output`) | 10 | Tool discovery |
| `event_msg` / `turn_aborted` | 1 | An interrupted turn |

### `session_meta`

Written first in every rollout, and the only place the session's identity and
working directory appear:

```json
{"session_id": "<uuid>", "id": "<uuid>", "timestamp": "<ISO 8601>",
 "cwd": "<absolute path>", "cli_version": "<version>",
 "originator": "<client>", "source": "<client>", "model_provider": "<provider>",
 "base_instructions": {"text": "<system prompt>"}}
```

`base_instructions` holds the full system prompt and is by far the largest field
in the file.

## What the adapter can and cannot supply

| Recall's model | Codex | Where from |
| --- | --- | --- |
| User messages | yes | `message` with role `user` |
| Assistant responses | yes | `message` with role `assistant` |
| Tool calls and results | yes | `function_call` / `custom_tool_call` and their outputs |
| Commands executed | yes | the `cmd` argument of an `exec_command` call |
| Files read or modified | no | not recorded as file operations |
| Start and end timestamps | yes | the envelope timestamp on the first and last record |
| Model identifier | usually | `turn_context.model` |
| Working directory | yes | `session_meta.cwd` |
| Git branch or commit | no | not recorded at all |

Two of these are worth stating plainly, because they differ from the first
adapter.

**Codex records the command line; Claude Code does not.** An `exec_command`
call carries `cmd` as a string, so a `Command` event here names what actually
ran. The Claude Code adapter has to leave that field empty and keep only what
the command printed. "What did this agent run against my machine" is therefore
answerable for Codex sessions in a way it is not for Claude Code ones.

**Codex records no git context; Claude Code does.** Claude Code puts
`gitBranch` on every record. Codex has no equivalent, so these sessions carry no
`GitContext` at all rather than a half-filled one. #32 derives repository state
from the repository itself, which is the right place for it.

The model is missing on 7 of the 24 sessions in the sample, because those
rollouts contain no `turn_context` record. That is reported as absent rather
than filled in from a neighbouring session.

`event_msg` records are not archived. They mirror the `response_item` stream for
Codex's own interface, and keeping both would store every message twice.
`encrypted_content` on a `reasoning` record is also dropped: it is opaque, it is
the largest field on the record, and nobody — Recall included — can read it
back. The reasoning *summary* beside it is kept.

## Stability

Rollouts in this installation were written by `cli_version` 0.42.0-alpha.3
through 0.152.1 — the oldest is from 2025 and still sits in the same tree. **A
single history mixes formats across a year of releases**, so tolerating the
unfamiliar is the normal case here, not an edge case.

The adapter therefore names only the fields it uses and ignores the rest. An
unrecognised record type is skipped; an unrecognised transcript item is kept as
`other` under whatever name Codex gave it, so a release that adds one costs a
release note rather than the user's archive.

Fixtures and their tests are #43.
