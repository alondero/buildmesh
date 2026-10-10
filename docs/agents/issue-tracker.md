# Issue Tracker

GitHub Issues on `alondero/buildmesh`. Use the `gh` CLI against `origin`.

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
