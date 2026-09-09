# Claude Code fixtures

Hand-written from the structure documented in
[`docs/providers/claude-code.md`](../../../../../docs/providers/claude-code.md).

**No real transcript is in here.** Every message, path and tool result is
invented. `.github/CONTRIBUTING.md` requires it, and a fixture that contained
someone's actual conversation would be a fixture nobody could safely share.

The shapes are real, though — they were confirmed against an installation
running Claude Code 2.1.215 to 2.1.228, which is why several of these look
stranger than a fixture written from imagination would.

| fixture | what it exercises |
|---------|-------------------|
| `ordinary.jsonl` | A normal session: messages, a tool call and its result |
| `bookkeeping.jsonl` | The 17 record types that are not transcript |
| `string_content.jsonl` | `message.content` as a string rather than an array |
| `text_tool_result.jsonl` | `toolUseResult` as a bare string, which dropped 1,061 real records before it was handled |
| `commands_and_files.jsonl` | Commands and file changes derived from `toolUseResult` |
| `thinking.jsonl` | Extended thinking blocks |
| `attachments.jsonl` | Internal reminders beside a real attachment |
| `unknown_shapes.jsonl` | Record types and block types this build has never seen |
| `malformed_middle.jsonl` | A broken line between good ones |
| `truncated.jsonl` | A file cut off mid-record |
| `no_timestamps.jsonl` | Records with nothing to date the session by |
| `empty.jsonl` | Nothing at all |
