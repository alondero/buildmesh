# 46. Trust for WSL linked worktrees opened from Windows

Status: accepted

## Context

Windows Git and libgit2 reject WSL-owned repositories unless their paths are
trusted. A protected `safe.directory` entry for a Mesh root does not cover its
linked worktrees. Homelab launches consequently created a checkout, then failed
before process launch when cross-runtime preparation reopened it. Reuse
validation also treated ownership errors as evidence of an invalid worktree.

## Decision

The Git worktree lifecycle derives exact worktree trust only on Windows, for
WSL UNC paths rejected with libgit2's typed ownership error. The parent must
still open through ordinary ownership validation. Before granting trust,
Buildmesh verifies the candidate's forward `.git` link, the parent's worktree
registration, the admin backpointer, and the shared common directory. Retained
administrative names after moves and relative forward links are supported.

`git::trust` writes exact `safe.directory` values for the candidate's two WSL
share spellings to `worktree-trust.gitconfig` in the app-data profile. It adds
one exact `include.path` value to Windows Git's global configuration. This is
protected Git configuration outside the repository and is shared by Windows
Git and libgit2. No process-wide ownership option or environment is changed.
Path aliases belong to `env::host_path`; distribution identity and guest path
case are retained. A root alias can resolve through an already trusted spelling
but is never automatically granted trust.

Config mutations use libgit2's lock-before-read writes and escaped, anchored
value matching. A bounded retry reopens the config after lock contention, so
concurrent processes preserve each other's entries. Empty `safe.directory`
reset values and user-managed includes remain intact. A final repository open
checks that the grants are effective; configuration overrides remain errors.

Creation grants trust after registration exists. A warm move trusts the source
before the move, revokes its managed grants after successful movement, and
trusts the newly verified destination before checkout. Successful removal revokes managed grants;
retained worktrees retain them. Revocation never edits user-managed
`safe.directory` entries. Cleanup failures are logged after disk removal.
Trust-write failures preserve the checkout and fail the operation for retry.
Reuse validation propagates access/configuration errors instead of calling the
directory an unregistered worktree.

## Alternatives

- Disabling ownership validation, even briefly: changes process-wide behavior
  while unrelated Git operations and launches run concurrently.
- Trusting a parent wildcard: broader than the verified checkout, and the
  bundled libgit2 does not implement Git's subtree wildcard behavior.
- Adding every grant directly to the user's global file: loses ownership of
  generated entries and makes safe revocation ambiguous.
- A process-global config search-path override: risks changing which user
  configuration every concurrent operation reads.

## Consequences and verification

The first affected launch adds the include; existing user trust needs no
migration. Removing the include revokes this profile's derived grants. A future
launch can recreate it only after verifying an accessible parent and candidate.
An uninstalled profile's missing include file grants nothing. Grants remain
ordinary persistent Git trust and are not a process sandbox.

Tests cover linkage validation, exact-value updates, and concurrent writers in
isolated child processes. The opt-in live WSL regression starts with root-only
trust and exercises creation, reuse, aliases, movement, guest Git access,
cleanup, and a failed trust write followed by retry. Test config search-path
overrides and Git environment overrides stay in the child process.

See [Windows development](../development/windows.md#wsl-worktree-trust) and
[recovery guidance](../troubleshooting.md#wsl-worktree-launch-fails-with-an-ownership-error).
