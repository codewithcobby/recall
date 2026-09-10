# Design notes

Written decisions that outlive a pull request: archive layout, the session model,
adapter contracts, and anything a future contributor would otherwise have to
reconstruct from the code.

## Contents

- [`archive-layout.md`](archive-layout.md) — what `.recall/` holds and why
- [`index-and-archive.md`](index-and-archive.md) — which of the two is the source
  of truth, and what may live in the database
- [`manual-testing.md`](manual-testing.md) — walking each phase by hand
- [`providers/`](providers/) — what each agent's session format really contains

Process and conventions live in [`.github/CONTRIBUTING.md`](../.github/CONTRIBUTING.md)
instead, and the security boundary in [`.github/SECURITY.md`](../.github/SECURITY.md).
