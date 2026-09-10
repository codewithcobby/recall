# Manual testing

Automated tests prove the code does what the code was written to do. This is for
the other question: does the thing behave sensibly when a person uses it.

Run the phase you are reviewing before merging its stack. Everything here works
against a throwaway directory — nothing touches a real project, a real provider
directory, or your home directory.

New phases append a section. Nothing above needs rewriting.

## Setup

Check out the branch you are reviewing. For a stack, that is the **top** layer,
which contains every layer below it:

```bash
cd /path/to/recall
git fetch origin
git checkout <top-branch-of-the-stack>
cargo build
```

Then work somewhere disposable, and keep the binary on hand:

```bash
mkdir -p /tmp/recall-demo && cd /tmp/recall-demo
printf 'target/\n' > .gitignore
RECALL=/path/to/recall/target/debug/recall
```

Clean up afterwards with `rm -rf /tmp/recall-demo`.

---

## Phase 1 — `recall init`

Implemented by #7, #8 and #9.

### 1. Initialize

```bash
$RECALL init
```

Expect the four paths it created, then a `.gitignore` suggestion. Exit code 0.

`index.db` is deliberately **not** created — the index appears when something
first needs it (#34).

### 2. Layout and permissions

```bash
find .recall | sort
ls -ld .recall .recall/sessions .recall/tmp
ls -l .recall/config.toml
cat .recall/config.toml
```

Directories must be `drwx------`, the config `-rw-------`.

**Worth checking by eye.** Archives hold conversation content, which routinely
includes proprietary source and occasionally a credential someone pasted into a
prompt. If these ever widen, other users on the machine can read it, and nothing
in the output of a normal run would tell you.

### 3. Run it again

```bash
$RECALL init
```

Expect `already initialized`, exit code 0. Not an error, and not a reset.

### 4. Archived sessions survive a re-run

**The most important check here.** The archive may be the only copy of a
conversation.

```bash
mkdir -p .recall/sessions/2026/09/08
echo "a precious conversation" > .recall/sessions/2026/09/08/abc.zst
$RECALL init && $RECALL init && $RECALL init
cat .recall/sessions/2026/09/08/abc.zst
```

Must still print `a precious conversation`.

### 5. Refusals

Each exits **1** and changes nothing on disk.

```bash
# a format this build does not understand
printf 'format_version = 99\n' > .recall/config.toml && $RECALL init
cat .recall/config.toml     # still 99 — the file it could not read is untouched

# not valid TOML
printf 'this is not toml {{{\n' > .recall/config.toml && $RECALL init

# config gone while sessions are archived
rm .recall/config.toml && $RECALL init
```

The third is the subtle one. With sessions archived and no config, the format of
those archives is unknown; writing a current-version config would claim
something Recall cannot know, and every archive under it would then be read
wrongly. So it refuses.

Remove the session and the same situation resolves the other way, because now
there is nothing to mislabel:

```bash
rm -rf .recall/sessions/2026
$RECALL init                # restores config.toml, exit 0
```

### 6. Your files stay yours

```bash
echo "my own notes" > .recall/notes.md
$RECALL init
cat .recall/notes.md
```

Expect `notes.md` reported under *Left alone*, and its contents unchanged.
Recall owns four entries inside `.recall/` and never touches anything else — see
[`archive-layout.md`](archive-layout.md).

### 7. `.gitignore` is never edited

```bash
$RECALL init | tail -2                      # suggests adding .recall/
printf 'target/\n.recall/\n' > .gitignore
$RECALL init | tail -2                      # suggestion gone
cat .gitignore                              # exactly what you wrote
```

Whether session history belongs in a project's git history is the user's
decision. Recall prints a suggestion; it must never act on it.

### 8. Something in the way

```bash
cd $(mktemp -d)
echo "not a directory" > .recall
$RECALL init
cat .recall
```

Refuses with exit 1, and the file is intact.

### 9. Commands that are not implemented yet

```bash
$RECALL sync ; echo $?
$RECALL sessions ; echo $?
```

Both exit **3** and name their tracking issue. Exiting 0 would tell a script the
work had been done.

### Phase 1 checklist

- [ ] `init` creates config.toml, sessions/ and tmp/, and no index
- [ ] Directories are `0700` and the config `0600`
- [ ] A second run reports "already initialized" and exits 0
- [ ] Archived sessions survive repeated re-runs untouched
- [ ] Unknown version, malformed config, and config-missing-with-sessions each exit 1 and change nothing
- [ ] A config lost from an *empty* archive is restored
- [ ] Unrecognised files are reported and left alone
- [ ] `.gitignore` is never modified
- [ ] A file named `.recall` is refused, not clobbered
- [ ] `sync` and `sessions` exit 3

---

## Phase 2 — the session model

Implemented by #10, #11 and #12.

Nothing here is reachable from the CLI. `Session`, `SessionEvent` and the JSON
Lines format are library types, and no command touches them until `recall sync`
in Phase 6. What Phase 2 settles is the *shape* of a session and how one is
written down — so the way to inspect it is to build one and look at it.

### 1. Run the example

```bash
cargo run --example session_roundtrip
```

It builds a session with every event variant, writes it, prints the result, reads
it back, and then tries to break it four different ways.

### 2. Read the metadata block

```text
  id            7d5e57020a19ce9b19891dcdc613cdd4
  provider      claude-code
  provider's id abc-123
  archive date  (2026, 9, 8)   <- UTC year/month/day
  duration      Some(150) minutes
  events        7
```

Two things to notice.

The **id is not the provider's id**. It is derived from the provider name and
their id together, and it is always 32 lowercase hex characters. That is what
makes it safe as a filename on every platform — a provider id can contain path
separators, characters Windows rejects, or differ only by case.

The **archive date is UTC**. The session starts at 12:00 UTC here, so it files
under `2026/09/08`. Change the start in the example to
`datetime!(2026-09-09 01:30:00 +05:00)` and re-run: the date must still be
`(2026, 9, 8)`, because 01:30 on the 9th in a +05:00 zone is the 8th in UTC.
Filing by local time would put the same session in different directories
depending on where the machine was.

### 3. Read the on-disk block

```text
  header {"format":1,"session":{"id":"7d5e5702…","provider":"claude-code",…
  event  {"kind":"user","at":"2026-09-08T12:00:05Z","content":"refactor the payment orchestrator"}
  event  {"kind":"tool_call","name":"read_file","arguments":"{\"path\":\"src/payments.rs\"}",…
  event  {"kind":"file_change","action":"modified","path":"src/payments.rs"}
```

**One line per event, and readable.** Both matter. Line-delimited is what lets
Phase 4 compress and decompress a session a piece at a time instead of holding a
whole transcript in memory. Readable is what lets someone inspect a damaged
archive by hand.

Note the tool call keeps its arguments verbatim, as text. Recall does not
reinterpret what an agent asked a tool to do.

### 4. Confirm the round trip

```text
  identical to what we wrote: true
```

Anything other than `true` is data loss.

### 5. Confirm damaged files are refused

```text
  tampered id      refused: session id … does not match the provider fields it should derive from …
  truncated file   refused: line 8 is not a valid session record
  future version   refused: session format version 99 is not supported (this build reads 1)
  empty file       refused: session file is empty
```

**This is the section worth reading carefully.** Every line must say `refused`.
An `ACCEPTED` anywhere is a bug, and a serious one: it would mean Recall handing
back a partial or wrong session as though it were the real thing, which is the
failure mode `.github/SECURITY.md` commits to never allowing.

The tampered-id case is the subtle one. The id is derived from the provider
fields stored beside it, so the two cannot disagree unless the file was edited or
corrupted. Checking on read turns a silently wrong archive into a loud one.

### 6. Break it yourself

Worth a few minutes, since these are the cases a line-delimited format gets wrong:

- Add an event whose `content` contains a newline. The output must still be one
  line per event — the newline is escaped, not emitted raw.
- Add an event whose content is itself JSON, such as
  `{"kind":"user","content":"hi"}` as a *string*. It must round-trip as text and
  not be mistaken for a record.
- Change `Provider::new("claude-code")` to `Provider::new("Claude Code")`. It must
  fail: provider names are lowercase letters, digits and hyphens only, so a name
  can never widen what an id or a path may contain.

### Phase 2 checklist

- [ ] The session id is 32 lowercase hex characters, not the provider's id
- [ ] The archive date is UTC, including for a session started just after midnight in a positive offset
- [ ] The header is one line and every event is one line
- [ ] Tool call arguments are stored verbatim
- [ ] The round trip reports `true`
- [ ] A tampered id is refused
- [ ] A truncated file is refused, naming the line
- [ ] An unsupported format version is refused
- [ ] An empty file is refused rather than read as an empty session

---

## Phase 3 — the local archive

Implemented by #13, #14 and #15.

Phase 2 defined what a session is. Phase 3 puts one on disk and gets it back, so
this is the first phase where conversation content is written anywhere.

Still no CLI surface — no command touches the archive until `recall sync` in #24
— so the example does the driving. It works in a temporary project that is
removed when it exits; nothing touches a real project or your home directory.

### 1. Run the example

```bash
cargo run --example archive_roundtrip
```

### 2. Where the session landed

```text
  session id   7d5e57020a19ce9b19891dcdc613cdd4
  archived at  .recall/sessions/2026/09/08/7d5e57020a19ce9b19891dcdc613cdd4.zst
```

The path is the whole design in one line: filed by **UTC date**, named by the
**derived id**, and the extension names the **encoding**. `.zst` is
Zstandard-compressed JSON Lines; archives written before #16 are `.jsonl` and
still read.

### 3. Permissions, all the way down

```text
  0700        .recall/
  0700        .recall/sessions/2026/09/08/
  0600        .recall/sessions/2026/09/08/7d5e5702….zst
  0700        .recall/tmp/
  staging holds 0 file(s)
```

**Every directory created on the way down must be `0700`, and the archive
`0600`.** A single `0755` anywhere in that chain means another user on the
machine can read archived conversations. This is worth reading line by line
rather than trusting.

`staging holds 0 file(s)` is the other thing to check. A file left in `tmp/`
after a successful write would mean the write was not as atomic as it claims.

### 4. Writing the same session twice

```text
  reported as new:      false
  bytes on disk changed: false
```

Both must be `false`. `recall sync` will run over the same sessions repeatedly,
and an archive that gets rewritten each time is an archive that can be corrupted
by a crash at the wrong moment. The second write reports "already present" and
touches nothing.

### 5. Reading it back

```text
  identical to what we archived: true
  events recovered:              6
```

Anything but `true` is data loss.

### 6. Damaged archives

```text
  truncated frame  refused: … is corrupt
                     <- incomplete frame
  garbage bytes    refused: … is corrupt
                     <- Unknown frame descriptor
  one flipped bit  refused: … is corrupt
                     <- Restored data doesn't match checksum
  future version   refused: … is not readable as a session
                     <- session format version 99 is not supported (this build reads 1)
  missing          refused: no archived session with id 7d5e5702…
  a directory      refused: … is not a file
```

Read the chain, not just the first line. The bottom of it is the useful part:
`Restored data doesn't match checksum` is the Zstandard frame checksum catching
a single flipped bit, which is the whole reason checksums are enabled.

Notice the two categories. **"is corrupt"** means the bytes on disk are damaged.
**"is not readable as a session"** means the archive is intact and what it
contains is something this build cannot read — a session from a newer Recall,
say. Conflating them would send someone to check their disk over a version
mismatch.

**Every line must say `refused`.** An `ACCEPTED` anywhere is a serious bug.

The `emptied` row is the one that matters most. If a zero-byte archive read back
as a session with no events, a caller would conclude the conversation *was*
empty rather than that it was lost — which is exactly the silent data loss
`.github/SECURITY.md` commits to preventing.

Note each refusal names both the session and the cause. "Missing" and "a
directory" are deliberately different: saying "no such session" when a directory
is sitting at the path would send you looking in the wrong place entirely.

### 7. One bad archive among several

```text
  archives found:    3
  read successfully: 2
  reported broken:   1
```

Three sessions archived, the middle one corrupted. The other two must still
read. The archive may be the only copy of the sessions that are still fine, so a
single damaged file must never take the rest down with it.

### 8. Break it yourself

- Change a session's start to `datetime!(2026-09-09 01:30:00 +05:00)`. It must
  file under `2026/09/08`.
- Add a `.md` file next to an archive and re-run. It must be ignored, not
  counted as a session.
- Flip a single byte in the middle of an archive. It must be refused: Zstandard
  frame checksums are enabled precisely so corruption cannot decode into
  something that looks like a session.

### Phase 3 checklist

- [ ] The session lands under `.recall/sessions/<UTC year>/<month>/<day>/`
- [ ] The archive is `0600` and every directory to it is `0700`
- [ ] Staging is empty after a successful write
- [ ] Writing the same session twice reports "not new" and changes no bytes
- [ ] The session reads back identical
- [ ] Truncated, emptied, garbage, future-version, missing and directory are all refused
- [ ] Each refusal names the session and the cause
- [ ] An emptied archive is never read as a session with zero events
- [ ] Two good archives still read when a third is corrupted

---

## Phase 6 — `recall sync`

Implemented by #24, #25, #26 and #27, plus the fix in #115.

The first phase with something to actually run. Everything before this built one
half or the other; `recall sync` is where they meet.

### 1. Sync a project you have used Claude Code in

```bash
cd /a/project/you/have/used/claude/code/in
recall init
recall sync
```

Expect something like:

```text
1 session found: 1 archived, 0 already had
```

If it says `No AI sessions found`, the sessions Claude Code has do not record
this directory as their working directory — check that you have actually run
Claude Code here.

### 2. Look at what landed

```bash
find .recall/sessions -name '*.zst'
du -sh .recall
```

One `.zst` per session, filed under its **UTC** start date and named by the
derived id, not Claude Code's.

### 3. Sync again

```bash
time recall sync
```

Expect `0 archived, 1 already had`, and expect it to be **much** faster than the
first run — on this repository, 0.006s against 0.305s. That gap is the point of
#25: an already-archived session is recognised from its id and never read at
all.

### 4. A session you are in the middle of

Run `recall sync` from a project while a Claude Code session is open and
active in it:

```text
1 session found: 0 archived, 0 already had
  1 still being written — left for a later run, so nothing is archived half-finished
```

**This is the behaviour worth understanding.** Archives are never rewritten and
an archived session is skipped without being read, so capturing a conversation
mid-flight would freeze its first half and lose the rest permanently. Sync waits
until the file has been quiet for five minutes.

### 5. Another project's sessions stay out

Sync in project A; sessions from project B must not appear in A's archive.
Sessions are matched by the working directory recorded inside them, and a
subdirectory of the project counts while a sibling with a similar name does not.

### 6. The provider's files are never touched

```bash
ls -l ~/.claude/projects/<a-project>/
recall sync
ls -l ~/.claude/projects/<a-project>/
```

Sizes and modification times must be identical. Recall reads Claude Code's
history; it never writes to it.

### Phase 6 checklist

- [ ] `recall sync` in an uninitialized project refuses and says to run `recall init`
- [ ] A session from this project is archived as a `.zst` under its UTC start date
- [ ] The archive reads back as the same conversation
- [ ] A second sync archives nothing and is dramatically faster
- [ ] A second sync leaves the archive bytes identical
- [ ] A session still being written is reported and not archived
- [ ] Another project's sessions are not archived here
- [ ] One unreadable session is reported and the others still land
- [ ] `~/.claude` is unchanged after a sync

---

## Phase 7 — reading the archive

Implemented by #28, #29 and #30, plus #130 and #131.

The first phase where Recall gives anything back.

### 1. List what is archived

```bash
recall sessions
```

```text
ID        STARTED           EVENTS  PROVIDER     MODEL            BRANCH
2a0a31a8  2026-09-09 08:41    2450  claude-code  claude-opus-5    fix/198-deletion-lifecycle
ba21d510  2026-09-01 13:20   16063  claude-code  claude-sonnet-5  security/143-refresh-token
6eaeb489  2026-08-26 09:14       2  claude-code  —                —
```

Newest first. `EVENTS` shows `?` for archives written before the count was
recorded — re-sync to fill it in.

This reads **one line per archive**, not all of them. On a real archive of
eleven sessions and 63,190 events that is 17 ms against 697 ms.

### 2. Read one back

```bash
recall show 6eaeb489
```

Eight characters is enough; any unambiguous prefix works. An ambiguous one lists
the candidates and refuses rather than guessing.

```bash
recall show ba21d510 --summary     # metadata only
recall show ba21d510 | less        # a large transcript
```

Nothing is truncated, so a 16,000-event session produces about 145,000 lines.
It streams, so that costs roughly 12 MB of memory rather than growing with the
session.

### 3. Check the archives are intact

```bash
recall verify
```

```text
11 sessions checked: 11 readable, 0 damaged
```

**Worth understanding:** step 1 cannot tell you this. Try it —

```bash
cp -R .recall /tmp/damaged && cd /tmp/damaged
# flip a byte in the middle of an archive
recall sessions    # still reports every session
recall verify      # reports the damaged one, exits 7
```

The listing validates only each archive's first line. That is the price of it
being fast, and `recall verify` is the deliberate, expensive alternative.

### 4. Exit codes

```bash
recall sessions ; echo $?     # 4 outside an initialized project
recall show zzzz ; echo $?    # 5
recall verify ; echo $?       # 7 if anything is damaged
recall --help ; echo $?       # 0
```

A person reads the message; a script reads the code. `recall --help` lists them.

### Phase 7 checklist

- [ ] `recall sessions` lists archived sessions, newest first
- [ ] Event counts appear for sessions archived by the current build
- [ ] `recall show <prefix>` prints a conversation with its tool calls
- [ ] An ambiguous prefix lists candidates and refuses
- [ ] `--summary` prints metadata without the transcript
- [ ] A large session renders without memory growing with its size
- [ ] `recall verify` reports a healthy archive as readable
- [ ] `recall verify` catches damage that `recall sessions` cannot see, and exits 7
- [ ] Exit codes match the table in `recall --help`
