# 2. Allow Branched Worktree Creation on Dirty Mesh

We will allow spawning new **Agent Nodes** in `branched` worktree mode even if the parent **Mesh** has uncommitted changes.

## Context

Previously, Buildmesh blocked the creation of `branched` worktrees if the parent repository was dirty to prevent perceived "state pollution." However, Git natively handles `git worktree add` from a dirty base repository safely: the uncommitted changes in the parent repository do not copy or bleed into the new worktree directory, which receives a completely clean checkout of the base commit. Enforcing a clean parent repository was an artificial constraint that interrupted the user's workflow.

## Decision

We will:
1. Remove the cleanliness check (`check_source_branch_clean`) during `branched` Agent Node spawning.
2. Allow worktrees to be created directly from the resolved base reference, regardless of the dirty state of the parent Mesh.

## Consequences

- **Pros:** Users can spawn new Agent Nodes instantly without being forced to commit, stash, or discard their active changes in the parent Mesh.
- **Cons:** None. Git's internal worktree architecture guarantees isolation, so there is no risk of bleeding uncommitted changes.
