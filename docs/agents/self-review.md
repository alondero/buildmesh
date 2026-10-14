# Self-review before handoff

Independent reviewers spend most of their rounds on the same few defect classes. Each one is cheaper to catch while you still have the code open than after a publish, a review and a repair round. Apply this list to your own diff after `npm run verify` and before you commit.

`npm run verify` prints the one-line form of each item below, plus the mechanical flags. `npm run self-review -- --base <commit>` prints the same without running any gate, which suits a repair round. The flags are advisory: they never fail a gate, and a flagged line can be correct. They list where to look.

1. **Claims.** Each absolute claim (every, never, unchanged, by construction, nothing, cannot) needs the line or test that makes it true, or soften it.
   This covers comments, docs, test titles, commit messages and PR text. State a rule once, at the code or document that owns it, and link to it from elsewhere; copies drift apart and the next review finds the stale one. In a repair round, delete a disputed claim rather than adding a longer defence: each new sentence is audited again, which is how a review loop stretches to six rounds.
2. **Sibling paths.** For each guard or invariant you add, find the other callers of the raw operation and route them through it, or say why one is exempt.
   Typical misses: manual retry beside the automatic drain, handover beside dispatch and nudge, a desktop command beside the mobile route, a new enum variant absent from a `match` or list, a frozen or persisted configuration that bypasses the new setting. A type that cannot be built without the guard beats a convention.
3. **Tests prove the change.** Each new test must fail when the production line it protects is reverted; check the one or two that matter by commenting the line out.
   A test that calls a helper production never calls, compares a constant with its own definition, or mocks the behaviour under test proves nothing. Cover the branch that is new (the second drain, the claimed path, the 32-bit process), not only the happy path.
4. **Outcomes stay distinct.** Do not fold gone, failed, not-found and read-error into one result that the caller reports as success.
   Avoid `.ok()`, `unwrap_or_default()` and matching on error text where the caller then carries on as if the call worked: return or log the error. A validation failure is not an authorization failure.
5. **State and time.** Backend state changes need the matching frontend event, the clock is read when used (not at mount), and a reset must not fire early.
   A counter or strike budget that resets before the window it guards has ended never accumulates; a persisted value cleared in the database but not in the store keeps being resent.
6. **Platform.** Compute platform-dependent test expectations (binary names, paths, shells, line endings) per platform; a green Windows run does not prove Linux CI.
7. **One concern per commit.** Formatting-only hunks go in their own `style(...)` commit; the message and PR body must match the diff and say what was not run.
   A known defect you document but do not fix needs a tracked issue and a test that reproduces it.

## What the flags look for

- **Absolute claims.** Added comment, documentation and test-title lines containing one of the claim words above. Documents are listed first, then doc comments, then test titles, then other comments, at most two per file before any file gets more; the list is capped at 12, and `--all` prints the rest.
- **Formatting-only hunks.** Commits (and uncommitted work) where at least 10 hunks differ from the old text only in whitespace, line wrapping or trailing commas, unless the commit subject is a `style`, `format` or `fmt` commit. Reviewers read every hunk, so a feature commit that is half rewrapping costs each review round more and hides the behaviour change. The `rust-format` gate checks the whole crate and fails on any rustfmt diff (#2022), and it checks only the final tree, so commit the behaviour change first, then run `cargo fmt --all` and commit the result alone as `style(rust): format <file>`.
