# Engineering and verification contract

Shared workflow for implementation and review across coding harnesses. Read the
conditional references below for the scope you are changing.

## Before changing code

1. Confirm the worktree root, branch and status; record HEAD as the comparison
   base. Keep edits under that root and confirm them with `git diff`.
2. Record the requested behavior, observable acceptance criteria and planned edits
   with the [development harness](development-harness.md#start-and-resume).
   Resume with `npm run harness -- status`; keep receipts machine-written.
3. Find the area in [the architecture index](../knowledge-primer.md), then read
   its owner document by section and confirm the contract against the code.
   For new providers, commands, routes or user workflows, follow the
   [common-change checklist](../development/README.md#common-change-checklists).

## Select the reference

| When | Read |
|---|---|
| Designing a behavior change or its regression tests | [Design for evidence](engineering-reference.md#design-for-evidence): async ordering, DB evolution, IPC, lifecycle and layout |
| Choosing or reporting checks | [Checks by scope](engineering-reference.md#choose-checks-by-scope) and [handoff evidence](engineering-reference.md#evidence-at-handoff) |
| Preparing a Windows checkout or shared build cache | [Worktree setup](development-harness.md#worktree-setup) |
| Changing instructions, hooks or CI gates | [Enforcement and maintenance](engineering-reference.md#enforcement-and-maintenance) |
| Reviewing code or repairing findings | [Review skill](../../.claude/skills/review/SKILL.md), [review and repair policy](development-harness.md#review-and-repair), and [self-review](self-review.md) |
| Running checks without writing into the reviewed checkout | [Isolated review checks](development-harness.md#isolated-review-checks) |
| Pushing, merging or checking remote CI | [Verify skill](../../.claude/skills/verify/SKILL.md), Remote CI section |

## Verify and hand off

Iterate with `npm run harness -- check` and focused behavioral tests. Run
`npm run verify` on the final candidate; apply the self-review prompts it prints.
A successful harness `finish` requires current verification, one evidence entry
per acceptance criterion, independent approval for the same tree, and no open
blockers. Missing prerequisites, deadlines and code failures remain distinct.

Report the commands actually run, outcomes, test counts and evidence limits.
Compilation and mock rendering do not prove real backend behavior. Attribute a
failure to the baseline only after reproducing it at the recorded base under
equivalent conditions. Keep incomplete acceptance visible at handoff.

Documentation changes follow [the documentation impact map](../documentation-standards.md#update-contract).
Before committing, inspect the diff and staged contents, and run
`npm run check:agent -- --base <recorded-base>` so committed changes are covered.
