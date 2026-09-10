# Codex fixtures

Hand-written from the structure documented in
[`docs/providers/codex.md`](../../../../../docs/providers/codex.md).

**No real transcript is in here.** Every message, path and tool result is
invented. `.github/CONTRIBUTING.md` requires it, and a fixture that contained
someone's actual conversation would be a fixture nobody could safely share.

The shapes are real, though — they were confirmed against an installation
running Codex CLI 0.152.1, whose history also held rollouts written as far back
as 0.42.0-alpha.3.

Each file is copied into a fake `~/.codex/sessions/2026/08/03/` tree under a
`rollout-<timestamp>-<uuid>.jsonl` name, so the tests exercise discovery,
parsing and normalization together rather than any one of them alone.

| fixture | what it exercises |
|---------|-------------------|
| `ordinary.jsonl` | A normal session: header, turn context, messages |
| `tool_calls.jsonl` | A shell command, an image tool, a custom tool, and their outputs |
| `bookkeeping.jsonl` | The `event_msg` and `world_state` records that are not transcript |
| `reasoning_and_roles.jsonl` | Reasoning beside opaque `encrypted_content`, a `developer` message, and the system prompt |
| `unknown_shapes.jsonl` | Record types, item types and fields this build has never seen |
| `malformed_middle.jsonl` | A broken line between good ones |
| `truncated.jsonl` | A file cut off mid-record, as one being written right now is |
| `no_timestamps.jsonl` | Records with nothing to date the session by |
| `empty.jsonl` | Nothing at all |
