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
