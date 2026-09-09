<!--
Title must follow Conventional Commits with a scope, e.g.
  feat(adapter): add Codex session discovery
Scopes are listed in .github/CONTRIBUTING.md > Commit Standards.
-->

## Summary

<!-- What changed and why. Two or three sentences. Not a changelog of your commits. -->

## Related issue

<!-- Required. One issue per PR. -->
Closes #

## Changes

-
-
-

## Type of change

- [ ] Bug fix (no behaviour change beyond the fix)
- [ ] New feature
- [ ] New or updated provider adapter
- [ ] Breaking change (CLI surface, archive layout, index schema, or public API)
- [ ] Refactor (no behaviour change)
- [ ] Performance
- [ ] Documentation
- [ ] Build, CI, or tooling

## Verification

<!-- Paste the commands you ran and their result. "Tests pass" without output is not verification. -->

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

<!-- For perf work, include before/after numbers. For adapter work, state which real session
     fixtures you parsed and how many. -->

## Archive and data safety

Recall's archive is the user's only copy of some conversations. Confirm each, or explain why it does not apply:

- [ ] No change to the on-disk archive layout, or the change is backward compatible with existing archives.
- [ ] No change to the index schema, or a migration is included and tested against a populated index.
- [ ] Provider session files are still treated as read-only.
- [ ] No conversation content is written to logs, stdout, or error messages that were not already user-requested.
- [ ] Nothing in this change sends data off the machine.

## Breaking changes and migration

<!-- Delete this section if the box above is unticked. Otherwise: what breaks, and what a
     user with an existing .recall/ directory must do. -->

## Checklist

- [ ] Branch name follows `<type>/<issue-number>-<slug>` (see CONTRIBUTING).
- [ ] Commits follow Conventional Commits with a scope, and carry no `Co-authored-by` or `Signed-off-by` trailers.
- [ ] `cargo fmt`, `cargo clippy -D warnings`, and `cargo test --workspace` all pass locally.
- [ ] Tests added: a regression test for a fix, unit and integration coverage for a feature.
- [ ] Public items are documented; `cargo doc` produces no new warnings.
- [ ] `README.md` and `--help` text updated if user-facing behaviour changed.
- [ ] Self-review completed — no leftover debug output, dead code, or commented-out blocks.
- [ ] No secrets, credentials, or real conversation content committed (fixtures are synthetic or redacted).
