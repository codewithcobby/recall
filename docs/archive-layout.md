# The `.recall/` archive layout

Settled before anything writes to it, because changing it afterwards means a
migration. Implementations must not assume anything about `.recall/` that is not
written down here.

## Layout

```text
.recall/
├── config.toml                     format version and project settings
├── sessions/
│   └── <YYYY>/<MM>/<DD>/
│       └── <session-id>.zst        one archived session
├── tmp/                            staging for atomic writes
└── index.db                        derived metadata index
```

## Ownership

Recall owns exactly four entries. Everything else inside `.recall/` belongs to
whoever put it there.

| Path | Owner | May Recall overwrite it? |
|------|-------|--------------------------|
| `config.toml` | Recall | Only through an explicit migration |
| `sessions/` | Recall | New files only; an existing archive is never rewritten |
| `tmp/` | Recall | Freely — nothing here is durable |
| `index.db` (and its `-wal` / `-shm` siblings) | Recall | Freely; it is derived and rebuildable |
| anything else | the user | Never. Not read, not moved, not deleted |

The last row is the one that matters. A file Recall does not recognise inside
`.recall/` is left exactly where it is. `recall init` reports it and continues;
no command deletes it.

## Date partitioning

A session is filed under its **start timestamp in UTC**, not local time. Local
time would put the same session in different directories depending on where the
machine was, which makes an archive non-reproducible and breaks any tooling that
walks the tree by date.

`<YYYY>/<MM>/<DD>` are zero-padded.

## Session filenames

`<session-id>.zst`, where the session id is Recall's own identifier for the
session, not the provider's.

The provider's id goes through unchanged only if it happens to be safe, which
cannot be assumed: provider ids may contain path separators, characters Windows
rejects, or differ only by case, which collides on a case-insensitive
filesystem. The id is therefore derived deterministically from the provider name
and its session id, and the derivation must produce a filesystem-safe string on
every supported platform.

The exact derivation belongs to the `Session` type in #10. This document only
fixes the constraint it has to satisfy.

## Atomic writes

`tmp/` exists because `rename(2)` is only atomic within a single filesystem.
Staging a session in the system temp directory and renaming it into `sessions/`
would silently degrade to a copy — and stop being atomic — whenever the two live
on different mounts, which is common enough to design against.

So: write to `tmp/`, fsync, rename into place. Anything left in `tmp/` is
wreckage from an interrupted run and may be deleted without asking.

## Format version

`config.toml` carries a single integer:

```toml
format_version = 1
```

It is read before anything else in `.recall/` is touched. A version Recall does
not understand is a hard error naming the version it found and the version it
supports — never a best-effort read, because guessing at an unknown layout is
how an archive gets corrupted.

Adding an optional key is not a version bump. Changing the meaning of an
existing key, moving a file, or changing how sessions are encoded is.

## Permissions

On Unix, `.recall/` and its directories are created `0700`, and files `0600`.
Archives hold conversation content, which routinely includes proprietary source
and occasionally a credential someone pasted into a prompt, so other users on
the machine must not be able to read them.

This includes files in `tmp/`. An archive written through a world-readable
staging file was never private.

Windows inherits the parent ACL; matching this guarantee there is tracked in #62.

## What survives losing a file

- **`index.db` deleted** — nothing is lost. It is derived and rebuilt from the
  archives. This is what keeps the index a cache rather than a second source of
  truth.
- **`config.toml` deleted** — the format version is unknown, so Recall stops
  rather than assuming the current one.
- **A session archive deleted** — that conversation is gone. There is no second
  copy. This is why writes are atomic and existing archives are never rewritten.

## Related

- `.github/SECURITY.md` — untrusted input, permissions, fail-closed integrity
- #7 `recall init`, #9 idempotency
- #34 index schema, #37 the index/archive boundary
