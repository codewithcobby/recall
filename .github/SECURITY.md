# Security Policy

Recall reads, normalizes, and stores the full transcripts of AI coding sessions. Those
transcripts routinely contain proprietary source code, internal architecture, file paths,
and sometimes credentials that were pasted into a prompt. Recall's archive is therefore a
high-value target sitting inside a developer's repository, and we treat it that way.

## The security boundary

```text
Provider session files
        │
        │  untrusted input
        ▼
   Recall adapters
        │
        │  normalized data
        ▼
   Recall archive
        │
        ├── sessions/*.zst
        └── index.db
```

**Provider session files are untrusted input.** Recall must treat every session file,
path, filename, metadata field, and serialized value in them as attacker-controlled data
— even though they were written by a tool the user installed themselves. A transcript can
contain anything the agent was shown, including content pasted from the internet.

Concretely, a path-like string inside a session (`../../../../etc/something`, an absolute
path, a Windows device name, a path containing a null byte) is *data to be recorded*, never
a path Recall resolves, reads, or writes. Recall derives its own destinations from the
project root and the archive layout, and validates that every path it touches stays inside
the boundary it intended.

The guarantee this adds up to:

> Recall can read potentially sensitive, untrusted session data, but it must never let
> that data escape the boundaries the user intended.

## Supported versions

Recall is pre-1.0. Security fixes are currently applied to the latest release and `dev`.
Older releases are not routinely maintained. This is the current policy, not a long-term
support commitment; it will be revisited at 1.0.

## Reporting a vulnerability

**Do not open a public issue for an exploitable vulnerability.**

Report it privately through
[GitHub Security Advisories](https://github.com/codewithcobby/recall/security/advisories/new).

Include: affected version or commit, the class of problem, the impact, and the minimum
detail needed to reproduce. Do not include real conversation content — synthesize a
fixture instead.

You can expect an acknowledgement within 7 days and an assessment within 30. We will tell
you when a fix ships and credit you in the advisory unless you ask us not to.

For hardening suggestions and non-exploitable concerns, use the
[Security hardening](https://github.com/codewithcobby/recall/issues/new?template=5-security.yml)
issue template instead — those are safe to discuss in public.

## In scope

- Reading a crafted provider session file causing code execution, path traversal, or a
  write outside the archive directory.
- Crafted files, directories, or **symlinks** causing Recall to read from or write outside
  the intended provider and archive boundaries. Following an attacker-controlled link out
  of those boundaries is a vulnerability, most relevantly during `recall sync`.
- Any path by which archived conversations leave the machine.
- Archive or index corruption that destroys previously captured sessions, or that Recall
  fails to detect (see *Integrity* below).
- Archive or index files readable by other users on the machine.
- Recall writing conversation content to a location the user did not ask for.
- Modification or deletion of a provider's own session files.
- Dependency or release-pipeline compromise.

### File permissions

Recall creates archive and index files with permissions that prevent other users on the
machine from reading conversation content, subject to what the platform supports. `.recall/`
can hold proprietary source and credentials that were pasted into a prompt; it is created
private, and a change that widens those permissions is a vulnerability.

### Integrity

**Recall fails closed when archive integrity cannot be verified.** A corrupt or truncated
archive is never silently treated as valid, silently truncated to the readable prefix, or
replaced with empty content — it is reported as an error naming the affected session, and
the rest of the archive stays readable. This pairs with the rule in
[CONTRIBUTING.md](CONTRIBUTING.md) that a malformed session file is an error value, never
a panic: one unreadable session fails that session, not the run.

## Out of scope

- Secrets that were already present in the AI session you archived. Recall does not scan,
  redact, or remove secrets from archived conversations unless an explicit feature is
  introduced to do so; it preserves what the agent recorded. Treat `.recall/` with the same
  care as the source tree it sits in.
- Anything requiring an attacker who already has read and write access to your home
  directory — at that point the provider's own session files are exposed regardless.
- Vulnerabilities in the AI coding agents themselves. Report those to their vendors.

## Handling session data in this repository

- Never commit real transcripts. Test fixtures must be synthetic or fully redacted.
- Never log conversation content at any level. Log identifiers, counts, and paths.
- Adapters open provider files read-only. A PR that writes to a provider's directory will
  be rejected.
