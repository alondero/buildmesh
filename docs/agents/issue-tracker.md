# Issue Tracker

GitHub Issues on `alondero/buildmesh`. Use the `gh` CLI against `origin`.

## Read a PR or issue

From the worktree, prefer one compact snapshot before issuing separate reads:

```powershell
npm run gh -- pr 2699
npm run gh -- issue 2623
```

Requires Node/npm and `gh` on PATH with its usual GitHub login (`gh auth login`).
The command uses origin's owner/repository, or `GH_REPO` when set, on github.com.
It makes one `gh api graphql` request, without caching or follow-up pagination.
PR output includes number, title, URL, state, head ref, draft/review decision,
labels, check rollup and conclusion counts, recent comment bodies and reviews.
Issue output includes number, title, URL, state, a body excerpt, labels and
recent comment bodies.

The snapshot is bounded to one screen: 20 labels, the latest 3 conversation
comments and latest 2 PR reviews, with each output line capped at 120 characters.
Text ending in `...` is shortened; collection counts show displayed/total.
Checks use the latest commit's overall rollup state, sample up to 100 check runs
and legacy statuses, and show up to 4 non-success check names. Conclusion counts
describe the sample; a missing rollup is `NONE`, never assumed green. Inline
review comments/threads are not included. Use `gh pr view <n> --comments`,
`gh issue view <n> --comments`, `gh pr checks <n>` or a targeted `gh api` read
when the snapshot indicates more detail is needed.

Exit codes: 0 is a successful read (including pending/failed CI), 1 is a missing
CLI, authentication, API or response failure, and 2 is invalid usage. Failures
write a short diagnostic to stderr; check `gh auth status` and the repository
remote, then retry with `gh` directly for details.

## Creating an issue

Search open issues first (`gh issue list --state open --search "..."`). Do not file a duplicate.

Write the body as UTF-8 with no BOM and pass it with `gh issue create --body-file`. Do not pass an inline `--body` from PowerShell: non-ASCII characters get corrupted.

An issue an agent can carry out states the problem, the evidence (file paths), the work, what is out of scope, and the acceptance check.

## Labels

[Triage labels](triage-labels.md) are the vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`.

A new issue starts as `needs-triage`. Do not mark `ready-for-agent` unless the acceptance check can be done without another product decision.

This repository does not ship a `to-issues`, `triage`, or `to-prd` skill. Those names are not entrypoints here.

## Comments and edits

`gh issue edit --comment` is not a real command. Post a comment by writing `{"body": "..."}` as UTF-8 JSON with Node (no BOM) and calling `gh api --method POST repos/alondero/buildmesh/issues/<n>/comments --input <file>`. Close with `gh issue close <n>`.

PowerShell `Set-Content -Encoding utf8` writes a BOM, which the GitHub API rejects.
