# Managed worktrees inherit scoped Git trust from their source

Status: accepted

## Context

Windows and WSL can report different ownership for the same files. Git CLI can
create or move a linked checkout successfully while embedded libgit2 rejects
its final path with `Owner`. A root exception or a pooled worktree's old name
does not satisfy libgit2's exact-path ownership validation after a rename.

## Decision

On ownership rejection, a managed checkout may inherit the authorization of its
source repository only after that source passes ordinary Git discovery and
ownership validation. Verify the checkout's forward link, common directory,
source registry entry and administrative backpointer before writing its
canonical exact path to protected global `safe.directory`. This applies to
creation, warm adoption, manual upgrade and restart.

Track exceptions Buildmesh inserts in
`buildmesh.managedWorktreeSafeDirectory`. Write the marker before the exception
so interruption cannot leave an untracked addition. Revoke exceptions before
their markers, and revoke only entries marked as Buildmesh-owned. Capture the
canonical path before a move/removal. Revoke the old exception before moving;
a config-write failure leaves the old checkout in place, and a failed Git move
restores its trust through the same validation guard. Removal retries failed
revocation through the existing removal queue. A pre-existing but ineffective user exception is a
configuration error, rather than permission to override the user's settings.

Read every protected Git config level when checking existing exceptions, and
write managed entries to the Global level. Cleanup opens that same level even
when an XDG config appears later, preserving user entries at other levels.

## Alternatives and consequences

Disabling ownership validation or trusting a folder wildcard broadens access
beyond the managed checkout. Requiring an exception for every new or renamed
Agent Node makes warm-pool adoption fragile. The selected approach preserves
normal validation and reuses the source's existing Git authorization, at the
cost of writing and maintaining exact entries in the user's global Git config.
Owned native checkouts require no new exception. This introduces no Mesh trust
setting, provider-runtime change or automatic authorization of an untrusted
source. Git configuration and filesystem writes remain outside DB locks.

## Verification and recovery

Security tests use real repository metadata with isolated config files. A live
Windows/WSL lifecycle regression runs alone with process-local libgit2 config
discovery redirected to a fixture; it must not change the user's Git config.
See [Windows ownership handling](../development/windows.md#wsl-path-mapping) and
[startup recovery](../troubleshooting.md#a-node-fails-to-start-and-the-reason-is-not-obvious).
