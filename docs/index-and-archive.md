# The index and the archive

Recall keeps a project's session history in two places. They are not equal, and
almost every question about the index is settled by knowing which is which.

| | `.recall/sessions/**.zst` | `.recall/index.db` |
|---|---|---|
| Holds | The conversation | Metadata about the conversation |
| Status | **Source of truth** | Derived cache |
| If deleted | Conversations are gone | Nothing is lost |
| Rebuilt from | Nothing — it is the original | The archives |
| Written by | `recall sync` | `recall sync`, `recall sessions --rebuild` |

The archive is the only copy of some conversations. The index is a shortcut for
finding them.

## The rules

**1. The archive holds conversation content. The index does not.**

No message text, tool input, tool output, file content, or command output is
written to the database. The index holds identifiers, timestamps, the provider
and model, the project and git fields, the event *count*, and the path to the
archive.

**2. The index is rebuildable from the archives alone.**

Everything in a row is recomputed by reading one archive's first line. Nothing
in the database exists only in the database.

**3. Deleting `index.db` loses nothing.**

It is recreated on the next `recall sync`, or by `recall sessions --rebuild`,
and the result lists exactly what it listed before.

**4. The archive wins.**

A session that has been archived is preserved regardless of what the index does
next. An index write that fails is reported, not retried into the archive's
path; the sync still succeeds. An index that cannot even be opened is set aside
and the sync goes on without it.

**5. An unusable index is replaced, never repaired.**

A file that is not a database, or one whose schema version this build does not
understand, is discarded and rebuilt. There is nothing in it to salvage, and
refusing to run until someone deletes a file by hand fails a command that has
everything it needs to succeed.

## Why the split is worth the complexity

A second store that has to agree with the first is real cost. It buys the
ability to answer "what sessions do I have" without decompressing everything.
On 2000 archived sessions totalling 16 MB:

| | |
|---|---|
| `recall sessions` (index) | 6.5 ms |
| Reading every archive's first line | 95.4 ms |

The gap widens with the archive, and the archive only grows.

Treating the index as a cache rather than a second source of truth is what keeps
that complexity bounded. A cache can be wrong, and the repair is always the same
— throw it away and rebuild. None of the usual questions about keeping two
stores consistent have to be answered, because one of them is disposable.

## How the rules are held

Documentation drifts. These are asserted in the test suite, and a change that
crosses the boundary fails:

Most live in `crates/recall-cli/tests/boundary.rs`, one rule per test:

| Rule | Test |
|---|---|
| 1 — no conversation content, through sync | `no_part_of_a_transcript_reaches_the_database` |
| 1 — the count is metadata, the events are not | `the_index_records_how_many_events_there_were_not_what_they_said` |
| 2 — rebuildable from the archives | `a_rebuilt_index_lists_exactly_what_the_archives_hold` |
| 3 — deleting the index loses nothing | `a_deleted_index_costs_nothing_but_time` |
| 4 — the archive wins | `a_corrupt_index_does_not_cost_the_user_a_session` |
| 5 — an unusable index is replaced | `an_index_from_a_newer_build_is_replaced_rather_than_refused` |

Rule 1 is also checked directly against the crate, in
`recall-index`'s `index_holds_no_conversation_content`.

Two details of how they are written matter:

- The content tests search the **bytes of `index.db`** for text that was in the
  transcript, rather than asking the index whether it stored any. The API would
  answer with the same assumptions that put the text there.
- The rebuild test compares **rows, not printed output**. A column the listing
  does not display could still be lost, and reading the table would not show it.

## Search: how the pressure was resolved

Content search (#38) meant matching text that lives in the archive, and the
obvious implementation — a full-text index in `index.db` — would have put that
text in the database and ended rule 1.

**Search reads the archives instead.** The index is untouched and stays
metadata-only. `recall search` does not use it at all, and works when it has
been deleted.

The measurement that decided it, on real transcripts:

| | 8 real sessions | 2000 sessions |
|---|---|---|
| archive | 5.4 MB | 16 MB |
| FTS5 index over the text | 34.7 MB (6.5x) | 170 MB (12.5x) |
| scanning the archives | ~90 ms | ~2.5 s |

A full-text index would have made the "small derived cache" several times larger
than the thing it indexes. Scanning costs time instead, and for the archives
people actually have it is fast enough not to notice.

This is a trade, not a free win: search is linear in the size of the archive, so
a very large archive will feel it. A full-text index can be added later as an
opt-in accelerator without changing what a search *means* — which is the part
that would be hard to take back. If that happens, rule 1 has to be rewritten
here first, and `index.db` inherits the archive's handling in
[`SECURITY.md`](../.github/SECURITY.md). Rules 2, 3 and 5 would still have to
hold.

The boundary that actually matters is **derived versus original**, not
*metadata versus content*. Rule 1 is the current, stricter position; rules 2, 3
and 5 are the ones that must never move.

## See also

- [`archive-layout.md`](archive-layout.md) — what `.recall/` contains
- [`.github/SECURITY.md`](../.github/SECURITY.md) — the security boundary
- [`.github/CONTRIBUTING.md`](../.github/CONTRIBUTING.md) — dependency direction:
  `recall-index` never depends on `recall-store`
