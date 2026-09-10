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

---

## Phase 8 — git awareness

Implemented by #31, #32 and #33.

Archived sessions now record which repository they ran in, alongside the branch
the provider already gave them.

### 1. Sync a project that is a git repository

```bash
cd /a/git/project/you/have/used/claude/code/in
recall init && recall sync
recall sessions
```

The `BRANCH` column should show real branch names.

### 2. Look at one

```bash
recall show <id> --summary
```

```text
project  /path/to/project
repo     /path/to/project
branch   fix/312-vendor-order
events   2082
```

`repo` is new. It is the repository root as git reports it, which is not always
the project directory — in a worktree it is the worktree.

### 3. The rule worth understanding

**A session records the branch it ran on, not the branch you are on now.**

Try it: note a session's branch, check out a different branch, then delete
`.recall/` and re-sync. The archived branch must not change. Claude Code records
what was true during the session.

Detection fills in the **repository root** and nothing else. It does not supply
a branch, even when the provider recorded none — see #163. A branch read at sync
time describes the moment it was read, not the session, so a session that
recorded no branch keeps none. That is the same rule commits already follow.

### 4. Cases that must not break a sync

- **Not a repository.** Plenty of work happens outside git. `recall sync` must
  archive normally, with `repo` and `branch` absent.
- **Detached HEAD.** `branch` must be absent, not `HEAD` — a branch called
  `HEAD` would collapse every detached session everywhere into one group.
- **No commits yet.** The branch has a name even with nothing on it, so it is
  recorded; no commit is invented.

### 5. Commits are deliberately absent

```text
recall show <id> --summary     # no commit fields
```

Claude Code records no commit, and a sync runs after a session ends — often days
later, possibly on a different branch. There is no deterministic way to say
which commit a session started or ended on, so nothing is filled in. #71 is
where a deterministic relationship gets worked out.

### 6. Nothing in your repository moves

```bash
git rev-parse HEAD ; recall sync ; git rev-parse HEAD
git status --porcelain
```

HEAD unchanged, and the only thing `git status` should show is `.recall/`.

### Phase 8 checklist

- [ ] A session from a git project records its repository root
- [ ] The branch is the one the session ran on, not the current one
- [ ] A branch missing from the provider stays missing, rather than being taken from the working tree
- [ ] A project outside git archives cleanly with both fields absent
- [ ] A detached HEAD records no branch
- [ ] A repository with no commits still records its branch
- [ ] No commit is ever recorded
- [ ] `recall sync` moves nothing in the repository

---

## Phase 9 — the SQLite index

`recall sessions` used to open the first line of every archive, every time. It
now answers from `.recall/index.db`. The point of testing it by hand is to
confirm the speed-up is real *and* that the database never becomes something you
would be sorry to lose.

### 1. The index appears, and sync fills it

```bash
recall sync
ls -la .recall/index.db
```

The file exists and is `-rw-------` — the same as an archive. Nobody else on the
machine can read it.

### 2. The listing still says the same thing

```bash
recall sessions
```

Identical to what Phase 7 produced: same columns, same order, newest first.
Nothing about the output changed — only where the answer came from.

### 3. It really is coming from the index

The convincing test is to take the archives away:

```bash
cp -r .recall /tmp/recall-backup      # so this is reversible
find .recall/sessions -name '*.zst' -delete
recall sessions
```

It still lists every session. It could only do that from the database.

Put them back before continuing:

```bash
rm -rf .recall && cp -r /tmp/recall-backup .recall
```

### 4. Deleting the index costs nothing

This is the property the whole design rests on.

```bash
recall sessions > /tmp/before.txt
rm .recall/index.db
recall sessions | tail -n +2 > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt && echo "identical"
```

It announces that it is rebuilding — the run is slower, and silence would look
like a hang — and then lists exactly what it listed before.

### 5. A broken index is replaced, not complained about

```bash
echo "not a database" > .recall/index.db
recall sessions
```

It rebuilds and lists your sessions. It does not ask you to delete a file, and
it does not fail. There was nothing in the database worth saving.

### 6. A broken index cannot cost you a conversation

The rule everything here follows is that the archive wins.

```bash
echo "not a database" > .recall/index.db
recall sync
```

New sessions are archived normally. If the index cannot be brought up to date,
sync says so and still succeeds — because the conversation is already safe.

### 7. No conversation content is in the database

```bash
strings .recall/index.db | grep -i "<a phrase you remember typing>"
```

Nothing. The database holds ids, timestamps, provider, model, project, git
fields, an event count and a path — never what was said. `docs/index-and-archive.md`
states the rules and the test suite holds them.

### 8. It is actually faster

```bash
time recall sessions
time recall sessions --rebuild
```

The second reads every archive; the first reads a database.

Do not expect to see anything on a handful of sessions — at eight sessions both
finish in about 3 ms and the difference is noise. The index earns its keep as the
archive grows, because reading every archive is work that scales with the archive
and querying the database is not. Measured on 2000 archived sessions totalling
16 MB: **6.5 ms against 95.4 ms**.

### Phase 9 checklist

- [ ] `recall sync` creates `.recall/index.db`, mode 600
- [ ] `recall sessions` output is unchanged from Phase 7
- [ ] The listing still answers with the archives deleted
- [ ] Deleting `index.db` rebuilds it and lists exactly the same sessions
- [ ] A file that is not a database is replaced without complaint
- [ ] A broken index does not stop `recall sync` archiving
- [ ] No transcript text appears in `index.db`
- [ ] `recall sessions` is materially faster than `--rebuild`

---

## Phase 10 — `recall search`

Search matches the text of the conversations themselves. It reads the archives
directly, so there is no index to prepare and nothing to keep in sync.

Start from a project with sessions already archived (`recall sync`).

### 1. Find something you remember

```bash
recall search "some phrase you remember"
```

You get the sessions that matched, newest first, with a short piece of the
surrounding text and the kind of event it came from. The ids are the ones
`recall show` takes.

### 2. Two words means both, close together

```bash
recall search retry backoff
```

Both words have to appear, and near each other. That second part matters: one
tool result can be a whole file, and without it a word on line 3 would match a
word on line 900 and give you pages of noise.

### 3. Quotes mean the exact phrase

```bash
recall search "refresh token"
```

Fewer, tighter results than `recall search refresh token`. Use quotes when you
know the wording.

### 4. Nothing found is a normal answer

```bash
recall search zzzznotpresent
echo $?
```

Says nothing matched, exits **0**. Finding nothing is an answer, not a failure.

### 5. Special characters are just characters

```bash
recall search "100%"
recall search "a*b"
recall search "; DROP TABLE sessions; --"
```

Each is searched for literally. There are no wildcards and no regular
expressions, and nothing is ever run as a query — so the last one just doesn't
match anything, and your archive is untouched.

### 6. An empty query is refused

```bash
recall search ""
echo $?
```

Exits **2** with a message. An empty query would match everything, which is
never what anyone meant.

### 7. It works without the index

```bash
rm .recall/index.db
recall search "some phrase you remember"
```

Same results. Search never reads `index.db` — that is why no conversation text
is stored there. Run `recall sync` afterwards to put the index back for
`recall sessions`.

### 8. A damaged archive is reported, not skipped

```bash
echo "broken" > .recall/sessions/<year>/<month>/<day>/<some-id>.zst
recall search "some phrase you remember"
```

The other sessions are still searched, and the damaged one is listed at the
bottom with a pointer to `recall verify`. Restore it with `recall sync` after
deleting the broken file.

### 9. Speed

```bash
time recall search "anything"
```

Search reads every archive, so it scales with the size of the archive, not the
number of matches. Roughly 90 ms on a 5.4 MB archive and 2.5 s on a 16 MB one.

### Phase 10 checklist

- [ ] A known phrase finds the right sessions
- [ ] Two words require both, near each other
- [ ] Quotes match the exact phrase
- [ ] No match says so and exits 0
- [ ] `%`, `*` and SQL text are matched literally and change nothing
- [ ] An empty query is refused with exit 2
- [ ] Search still works after deleting `index.db`
- [ ] A damaged archive is reported and the rest are still searched

---

## Phase 11 — the Codex adapter

Implemented by #40, #41, #42 and #43.

The first phase with a second provider in it. Most of what follows is checking
that Codex sessions behave like any other session — and that the two places
Codex genuinely differs from Claude Code are handled honestly rather than
papered over.

Codex keeps its history at `~/.codex/sessions/<YYYY>/<MM>/<DD>/`, partitioned by
date rather than by project. Nothing in that path says which repository a
session ran against, so a session is matched to a project by the `cwd` recorded
inside the rollout, exactly as Phase 6 describes.

### 1. Sync a project you have used Codex in

```bash
cd /a/project/you/have/used/codex/in
recall init
recall sync
```

Expect the usual `N sessions found: N archived, 0 already had`.

If it says `No AI sessions found`, the rollouts Codex has do not record this
directory as their working directory. To find one that does:

```bash
grep -l -m1 '"cwd":"'"$PWD"'"' ~/.codex/sessions/*/*/*/*.jsonl
```

### 2. Both providers land in the same archive

```bash
recall sessions
```

```text
ID        STARTED           EVENTS  PROVIDER     MODEL          BRANCH
9f2c1d40  2026-08-03 13:10      12  codex        gpt-5.5        —
2a0a31a8  2026-08-01 08:41    2450  claude-code  claude-opus-5  fix/198-x
```

Two things to look at.

`PROVIDER` says `codex`. Session ids are derived from the provider *and* the
provider's own id, so a Codex session and a Claude Code session can never
collide even if both agents used the same uuid.

`BRANCH` is `—` on every Codex row, because Codex records none and nothing
invents one — see step 4.

### 3. The command line is really there

**The thing Codex can do that Claude Code cannot.** Claude Code records what a
command printed but not what was run; Codex records the command itself.

```bash
recall show <a-codex-id> | grep -A2 'command'
```

A `command` event names the actual invocation — `cargo test --workspace`, not an
empty string. `exit_code` is absent, because Codex reports the outcome in the
tool's output rather than as a status, and inventing a `0` there would claim a
success it never stated.

### 4. Git context is absent, and stays absent

Claude Code puts `gitBranch` on every record; Codex records neither branch nor
commit. So a Codex session has no branch, and nothing supplies one.

```bash
recall show <a-codex-id> --summary
```

`repo` is present — the repository root is a fact about the path, and does not
change with time. `branch` and the commits are absent.

**This is what #163 fixed, and it is worth checking rather than assuming.**
Sync used to fill a missing branch in from the working tree, which meant a Codex
session from July was archived carrying whatever branch the repository sat on at
sync time. Confirm the column is not doing that:

```bash
git -C <the project path from --summary> branch --show-current
```

A Codex session must show **no** branch, whatever that prints. If it shows the
same value, detection is supplying it again and the regression is back.

The model can be absent too, on a rollout with no `turn_context` record. 7 of
the 24 sessions in the installation this was written against had none. `—` in
that column is the honest answer.

### 5. Reasoning is kept; the unreadable part is not

```bash
recall show <a-codex-id> | grep -c 'reasoning'
```

Codex records a readable reasoning summary next to an `encrypted_content` blob.
The summary is archived. The blob is not — it is opaque, it is the largest field
on the record, and nobody, Recall included, can ever read it back.

### 6. The same conversation is not archived twice

Codex writes each message twice: once as the transcript (`response_item`) and
once as an interface event (`event_msg`) for its own UI. Only the first is
archived.

```bash
recall show <a-codex-id> | grep -c 'assistant'
```

Compare against what you actually said and were told in that session. A number
roughly double what you remember would mean the bookkeeping stream is being
archived as conversation.

### 7. A rollout being written right now

Run `recall sync` while a Codex session is open in the project. Same rule as
Phase 6: it is reported as still being written and left for a later run, so
nothing is archived half-finished.

### 8. Codex's files are never touched

```bash
ls -lR ~/.codex/sessions | md5
recall sync
ls -lR ~/.codex/sessions | md5
```

Identical. Recall reads Codex's history and never writes to it — including the
`sessions/` tree, which it walks to a bounded depth and never follows a symlink
out of.

### 9. Search reaches Codex sessions too

```bash
recall search "something you said to codex"
```

Search is provider-agnostic; a Codex session is just another archive to it.

### Phase 11 checklist

- [ ] A Codex session from this project is archived
- [ ] `recall sessions` shows it with provider `codex`
- [ ] Codex and Claude Code sessions coexist in one archive without colliding
- [ ] A `command` event carries the real command line, with no invented exit code
- [ ] Branch and commits are absent on every Codex session, even in a git repository
- [ ] A session with no `turn_context` reports no model rather than guessing one
- [ ] Reasoning summaries are archived; `encrypted_content` is not
- [ ] Messages appear once, not twice
- [ ] A session still being written is reported and not archived
- [ ] `~/.codex` is unchanged after a sync
- [ ] `recall search` finds text from a Codex session
